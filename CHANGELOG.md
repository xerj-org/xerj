# Changelog

All notable changes to XERJ are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.0-rc.99] - 2026-10-24

Window since v1.0.0-rc.98: 116 merged pull requests. x86_64 release binaries
run on CPUs without AVX2. Cluster mode replicates index metadata, routes a
document write to the primary named at create, copies an acknowledged write
onto replicas placed at create, acknowledges that write once a quorum of
the copies has confirmed, reloads the shard plan from an on-disk Raft log
after a restart, installs a compacted Raft snapshot on a lagging member,
copies the primary's live documents onto a replica that is behind, and runs
`_bulk`, `_update` and `_update_by_query` on the shard primary;
[#1170](https://github.com/xerj-org/xerj/issues/1170) stays open. `ctutils`
0.4.5 closes RUSTSEC-2026-0385. Autoindex, corpus and search fixes landed,
including partial work on
[#1122](https://github.com/xerj-org/xerj/issues/1122); that issue stays open.
MCP gained local definition lookup and ordered batch retrieval. A DeepSeek
Harness profile bundle ships in `dsh/`. Contributors in this window:
Jarvis636431, Tong-bit-art, redwolf2019, ArkNill, pallidev,
samuelmbabhazi, elewashy, WYSTAF, kamwoh, mgtf, mariomarcolongo and Jun516.

### Upgrade notes

- **No downgrade after a case-distinguished field write** (PR
  [#2261](https://github.com/xerj-org/xerj/pull/2261)). Opening a data
  directory does not by itself change its format. The marker moves from
  2 to 3 only when this binary writes a field whose Windows case-identity
  is not the name itself: an ASCII name with an uppercase letter
  (`page_Id`), or `ı`, `ſ`, `K`, or `ß`. rc.98 supports format 2 and
  refuses a higher marker, so after that write it will not open the
  directory. Back up the data directory before upgrading if you may need
  the older binary. A directory that has not taken that write still
  opens on rc.98. v1 and v2 segments still open on rc.99.
- **Packed embeddings** (PR
  [#2240](https://github.com/xerj-org/xerj/pull/2240)). A semantic vector
  this binary writes is stored as one `xerj:f32:` hex string. That does
  not advance the data-dir format marker, so an older binary still opens
  the directory. It does not unpack the string: kNN does not score that
  vector, and a source read returns the hex instead of a number array.
  Vectors already stored as number arrays stay that way until rc.99
  rewrites them.
- **Hub catalogues** (PR
  [#2249](https://github.com/xerj-org/xerj/pull/2249)). rc.99 writes
  catalogue format 2, which adds optional `upstream` (`owner/name`).
  rc.98 and older clients accept only format 1, so they refuse a
  catalogue this binary writes. This binary still reads format 1. The
  published catalogue is still format 1 until a binary with this code
  rebuilds it, so `upstream` is empty there.
- **Re-index to pick up new extraction** (PRs
  [#2278](https://github.com/xerj-org/xerj/pull/2278),
  [#2342](https://github.com/xerj-org/xerj/pull/2342),
  [#2247](https://github.com/xerj-org/xerj/pull/2247)). Documents already
  in an index keep the extraction and settings of the binary that wrote
  them. An unchanged file is not re-extracted. `xerj autoindex --fresh`
  rebuilds the index. That is what applies the AND match operator and
  the asctime date mapping on a logs dataset (#2278), indexes `.sql`
  schema and routines as code (#2342), and skips catalog phrase text,
  parses phone and RFC 5424 syslog, and elects the english analyzer for
  a logs dataset (#2247).
- **Templates apply at create** (PR
  [#2283](https://github.com/xerj-org/xerj/pull/2283)). An index that
  already exists keeps the mapping and `index.default_pipeline` it was
  created with. A matching template's `semantic_text` and default
  pipeline are merged in when an index is created, including the first
  write that auto-creates one. Indices created before this are unchanged.
- **CPU baseline** (PR
  [#2253](https://github.com/xerj-org/xerj/pull/2253)). The four x86_64
  release targets are `x86-64-v2` (SSE4.2). The installers no longer
  refuse a CPU without AVX2. `XERJ_SKIP_CPU_CHECK`, the installer bypass
  added in PR [#2178](https://github.com/xerj-org/xerj/pull/2178), is
  gone. A CPU without SSE4.2 still cannot run the binary.
- **Defaults that moved:**
  - A host under 16 GiB gets a process cap of half its RAM. On a budget
    that cannot hold the old 512 MiB merge input, the merge batch is
    `budget/64` (64 MiB at a 4 GiB cap) and concurrent merges share a
    quarter of the budget. Hosts at 16 GiB and above keep the previous
    steps. This improves
    [#1122](https://github.com/xerj-org/xerj/issues/1122); the issue stays open
    (PR [#2340](https://github.com/xerj-org/xerj/pull/2340)).
  - On Windows the cap is then lowered to what this process can still
    commit, not only to physical RAM. Linux and macOS are unchanged.
    This improves
    [#1122](https://github.com/xerj-org/xerj/issues/1122); the issue stays open
    (PR [#2236](https://github.com/xerj-org/xerj/pull/2236)).
  - On a logs index that carries
    `index.query.default_match_operator: and`, an omitted `match`
    requires every token. An index without that setting, and an
    explicit `or` or `and`, are unchanged (PR
    [#2278](https://github.com/xerj-org/xerj/pull/2278)). Existing
    indices do not gain the setting until they are recreated.
  - When a request highlights a `semantic_text` field on a semantic
    query, the default highlighter is the semantic one: chunk fragments,
    not `<em>` tags. A request that does not ask for highlight is
    unchanged. An explicit type other than `semantic` stays on the
    lexical highlighter (PR
    [#2227](https://github.com/xerj-org/xerj/pull/2227)).
  - A `semantic` query on a wildcard, `_all`, an alias, or a comma list
    returns HTTP 200 and records the indices that lack the field under
    `_shards.failed`, instead of failing the whole request with 400. If
    every searched index is skipped, the request is still 400. One
    concrete index that lacks the field is still 400 (PR
    [#2308](https://github.com/xerj-org/xerj/pull/2308)).
  - `POST /{index}/_validate/query` reports `valid: false` for a body
    `_search` rejects: a non-numeric value on a numeric field, or a
    field that is neither indexed nor has doc values. A selector that
    resolves to no index stays parse-only (PR
    [#2263](https://github.com/xerj-org/xerj/pull/2263)).
  - A question with three or more content tokens ranks a hit that covers
    more of the question above a one-word symbol. A one-token lookup is
    unchanged (PR [#2329](https://github.com/xerj-org/xerj/pull/2329)).
  - Plain-string MCP `xerj_search`, when the caller does not set
    `_source`, also returns `ax_file` and `ax_locator`. An explicit
    `_source` list is left alone (PR
    [#2255](https://github.com/xerj-org/xerj/pull/2255)).
  - One indexed JSON document is capped at 8 MiB. That ceiling is a
    constant, not a setting (PR
    [#2256](https://github.com/xerj-org/xerj/pull/2256)). `PUT
    /{index}/_doc/{id}` and `PUT /{index}/_create/{id}` refuse a larger
    body before parsing it (PR
    [#2315](https://github.com/xerj-org/xerj/pull/2315)). An `_update`
    body over two 8 MiB documents plus 256 KiB of envelope is refused
    before `serde_json` (PR
    [#2322](https://github.com/xerj-org/xerj/pull/2322)). This improves
    [#1122](https://github.com/xerj-org/xerj/issues/1122); the issue stays open.
  - A bulk whose token analysis would cross the memory watermark is
    HTTP 429 before the documents are parsed (PR
    [#2302](https://github.com/xerj-org/xerj/pull/2302)). This improves
    [#1122](https://github.com/xerj-org/xerj/issues/1122); the issue stays open.
  - SIGTERM and SIGINT answer new HTTP requests with 503, leave
    `/health/live` at 200, and keep the ports bound until the final
    flush finishes. Connection refusal is the signal that the flush is
    done (PR [#2246](https://github.com/xerj-org/xerj/pull/2246)).
  - `engine.flush_admit_timeout_secs` (default 120) and
    `engine.flush_timeout_secs` (default 300) bound how long a flush
    waits for a permit and how long refresh waits for one index.
    `XERJ_FLUSH_ADMIT_TIMEOUT_SECS` and `XERJ_FLUSH_TIMEOUT_SECS`
    override them. A refresh that used to wait now returns when the
    budget is spent (PR
    [#2129](https://github.com/xerj-org/xerj/pull/2129)).

### Security

- **`ctutils` 0.4.5** (RUSTSEC-2026-0385, published 2026-10-23). `Choice`
  mask generation and 32-bit `u128` comparisons before 0.4.4 can branch on
  secret values. Nothing in the workspace names the crate. It is transitive:
  `digest` 0.11 depends on `ctutils ^0.4`, and that line is what `sha1`
  0.11, `sha2` 0.11, `hmac` 0.13 and `md-5` use, reaching `xerj-autoindex`
  and `xerj-storage` through the AWS SDK checksum path and `pdf_oxide`.
  `cargo update -p ctutils` locked 0.4.5 (0.4.4 is the advisory floor).
  `cmov` stayed at 0.5.4. Lockfile only (PR
  [#2268](https://github.com/xerj-org/xerj/pull/2268)).

### Added

- Cluster replication, nine slices of
  [#1170](https://github.com/xerj-org/xerj/issues/1170). The issue stays
  open. With `[cluster] enabled = false` every one of these paths is the
  previous local call.
  - Index create, delete, mapping and settings are proposed through the Raft
    log. The HTTP call succeeds only after this node has applied the
    committed entry. A follower forwards to the leader. No elected leader, a
    timeout, or leadership lost before commit is HTTP 503, and the index is
    not created locally. The accepting node applies index templates once and
    replicates the effective schema. Console system indices created before
    the handle is attached stay local. Document writes stay on the accepting
    node. #2257 left that log in memory. The on-disk log is the slice below
    (PR [#2257](https://github.com/xerj-org/xerj/pull/2257)).
  - The create entry names one primary per member (member ids sorted, shard
    `i` on the `i`th). `PUT` / `POST /{index}/_doc`, `PUT /{index}/_create/{id}`,
    `GET` and `DELETE /{index}/_doc/{id}` run on that primary. A search of
    one concrete index asks every owner and merges hits by per-shard score,
    then applies `from` / `size`. A forward that fails is 503 and is not
    also written locally (PR
    [#2271](https://github.com/xerj-org/xerj/pull/2271)).
  - `index.number_of_replicas` at create (nested, flat or dotted; absent
    means 0) places that many copies, capped at `members - 1`, in a hidden
    `.xerj_replica_{index}_{shard}` index so a search of the user index does
    not count the copy twice. This pull request waited for every placed
    replica before acknowledging the caller. The next slice acknowledges a
    quorum instead. GET, and a search whose primary does not answer, read a
    replica. There is no promotion: a write still needs the primary. A later
    `PUT /{index}/_settings` stores a new replica count and does not move
    copies (PR [#2287](https://github.com/xerj-org/xerj/pull/2287)).
  - With `[cluster] enabled = true`, a single-document index or delete is
    acknowledged when a quorum of the shard's copies has confirmed, not only
    when every placed replica has. Copies are the primary plus the replicas
    named by `index.number_of_replicas` at create. A quorum is
    `copies / 2 + 1`, and the primary already counts, so one replica out of
    two is enough.     The replica that did not confirm is removed from the
    in-sync set through the Raft log and is not used for a later read until
    the backfill slice below has copied the primary's live documents onto
    it. `number_of_replicas` 0 or 1 is unchanged: there is no replica to
    wait for, or that one replica must confirm. Fewer confirmations than
    the quorum is still HTTP 503 `replica_not_acknowledged`: the primary
    has the write, and retrying the index copies it to the replicas still
    in sync. Single-node mode does not take this path (PR
    [#2303](https://github.com/xerj-org/xerj/pull/2303)). #1170 stays open:
    there is still no `wait_for_active_shards` and no replica promotion.
  - With `[cluster] enabled = true`, the server opens an on-disk Raft log
    at `{data_dir}/_raft` and replays its committed prefix before it
    serves. A restart restores the shard plan and the in-sync set, so a
    later document write is routed to the primary again. A member that
    starts with an empty log copies the leader's log and applies it, which
    installs index metadata (and an empty replica store when this node is
    a replica). Documents written before it joined are copied by the
    backfill slice below. Term and vote are stored with the log, so a
    restarted node does not grant a second vote in the same term.
    `[cluster] enabled = false` does not open the log. Log compaction and
    InstallSnapshot are the next slice. This improves
    [#1170](https://github.com/xerj-org/xerj/issues/1170); the issue stays
    open (PR [#2313](https://github.com/xerj-org/xerj/pull/2313)).
  - A member compacts its Raft log once 64 metadata entries have been
    applied since the previous snapshot. The compacted prefix is stored in
    `{data_dir}/_raft/snapshot.meta` and removed from `raft.log`. A peer
    whose log does not overlap that prefix receives InstallSnapshot and
    applies those commands, which installs the same index metadata an
    AppendEntries catch-up would have. The snapshot is that command prefix,
    not a smaller folded image and not the documents. A history that does
    not fit in 8 MiB stays in the log. Restart replays the snapshot and
    then the entries after it. Copying the documents themselves is the
    backfill slice below. Part of
    [#1170](https://github.com/xerj-org/xerj/issues/1170); the issue stays
    open (PR [#2320](https://github.com/xerj-org/xerj/pull/2320)).
  - A replica that does not have the primary's live documents copies them
    and then rejoins the in-sync set. That covers a member that starts
    after the writes (the create entry opens an empty replica store), a
    replica removed from the in-sync set because it missed an
    acknowledgement, and a process that stops and later opens the same
    data directory: documents inserted, updated, or deleted while it was
    down are applied to its replica store before the enlarged set is
    published. The primary sends the live documents, drops ids the replica
    still has that the primary no longer does, and publishes the enlarged
    in-sync set through the Raft log. The primary holds that shard's
    replica lock across the copy, so a write that arrives during the copy
    is replicated to the new set when the lock is released. A later
    single-document index or delete is copied to that replica with the
    others. A write still needs the primary. Changing membership does not
    move copies. `[cluster] enabled = false` does not do this. Part of
    [#1170](https://github.com/xerj-org/xerj/issues/1170); the issue stays
    open (PR [#2328](https://github.com/xerj-org/xerj/pull/2328)). The
    outage case (insert, update and delete, then reopen the same data
    directory) is covered by the test-only pull request
    [#2341](https://github.com/xerj-org/xerj/pull/2341).
  - `POST /_bulk`, `POST /{index}/_bulk` and `POST /{index}/_update/{id}`
    run on the shard primary, whichever member accepted the request. A
    bulk body is split by primary. An action whose primary is another
    member is forwarded there and is not indexed on the receiver. The
    primary then copies the stored document (or the delete) onto the
    in-sync replicas and acknowledges it with the same quorum as
    `PUT /{index}/_doc/{id}`. `_bulk` covers index, create, update and
    delete. `_update` covers a partial document and a script; the script
    runs on the primary, where the current source is. A missing `_id` on
    index or create is chosen before the owner is selected, so the stored
    id is the id that was routed. An update that does not change the
    source is not copied again. A copy that misses quorum is
    `503 replica_not_acknowledged` for that item: the primary has the
    write. Native `/v1` ingest still writes on the receiving node.
    `_update_by_query` is the next slice. A write still needs the primary.
    `[cluster] enabled = false` does not take this path. Part of
    [#1170](https://github.com/xerj-org/xerj/issues/1170); the issue stays
    open (PR [#2344](https://github.com/xerj-org/xerj/pull/2344)).
  - `POST /{index}/_update_by_query` runs on every shard primary, whichever
    member accepted the request. Each primary updates the documents in its
    own shard and copies those updates to the in-sync replicas, with the
    same quorum as `PUT /{index}/_doc/{id}`. `max_docs` is one budget for
    the index, spent across primaries in node-id order, not once per shard.
    A primary that does not answer refuses the request before any document
    is written. A copy that misses quorum is HTTP 503
    `replica_not_acknowledged`: the primaries that ran have the update, and
    the response still reports `updated` and `failures`. `?pipeline=` runs
    on the primary; a missing or unrunnable pipeline there is HTTP 400 and
    writes nothing. Pipelines are not replicated. `wait_for_completion=false`
    uses the same path. `_delete_by_query` and native `/v1` ingest still
    write on the receiving node. A write still needs the primary.
    `[cluster] enabled = false` does not take this path. Part of
    [#1170](https://github.com/xerj-org/xerj/issues/1170); the issue stays
    open (PR [#2347](https://github.com/xerj-org/xerj/pull/2347)).
  - What #1170 still does not do: `wait_for_active_shards`; replica
    promotion (a write still needs the primary); native `/v1` ingest and
    `_delete_by_query` (they stay on the receiving node); replicating
    pipelines; moving copies when membership changes; merging aggregations,
    suggest, collapse, rescore, profile, `search_after` or scroll across
    owners; node-lifecycle APIs. The snapshot is the command prefix, not
    the documents, and a history that does not fit in 8 MiB stays in the
    log.

- A DeepSeek Harness profile bundle in `dsh/` (`xerj-dsh`, Apache-2.0,
  version `1.0.0-rc.99`) inserts the README's `mcp-xerj` stdio row via
  `@deepseek-ai/dsh-mcp-client` at 0.2.1-rc.3. The row is still untested on
  DSH. The English, Korean and Simplified Chinese READMEs link the bundle.
  `landing/brandbook/xerj-mark-400.png` is a 400×400 PNG of the letter mark
  for directory submissions (PR
  [#2298](https://github.com/xerj-org/xerj/pull/2298)).

- `xerj batch-search --input <file|->` and MCP `xerj_batch_search` retrieve
  up to 50 IDs or questions in input order through one `_msearch` call.
  Each item returns cited evidence, an explicit empty-result abstention, or
  an independent error. Exact IDs require an indexed keyword field. Optional
  whole-response token budgets preserve outcomes and at least one citation
  per matched item, or refuse explicitly. This is retrieval, not generated
  answers or calibrated confidence. Partial implementation of
  [#1801](https://github.com/xerj-org/xerj/issues/1801) (PR
  [#2333](https://github.com/xerj-org/xerj/pull/2333), Jarvis636431).

- MCP `xerj_def` exposes local definition lookup, completing
  [#2080](https://github.com/xerj-org/xerj/issues/2080) (PR
  [#2166](https://github.com/xerj-org/xerj/pull/2166), Jarvis636431). It
  shares `xerj def --json` query construction, mapping guards, ranking and
  location deduplication; accepts `symbol`, `prefix`/`index`, `kind`,
  `lang`, `k` (1–50) and `max_tokens`. No match is a successful empty hit
  list; invalid arguments are rejected before HTTP.

- MCP `xerj_search` accepts `prefix` as an alternative to `index`, matching
  `xerj search --prefix`: `project` searches `project-*`. Conflicting scope
  arguments and empty or non-string prefixes are rejected before HTTP
  requests. The default remains `ax-*` (PR
  [#2137](https://github.com/xerj-org/xerj/pull/2137), Jarvis636431; the
  prefix half of #2080).

### Fixed

- `xerj autoindex` no longer junks every PDF on an OS that rejects
  `setrlimit(RLIMIT_AS)` (macOS 27 returns `EINVAL`; a hard limit below
  1536 MiB returns `EPERM`)
  ([#2336](https://github.com/xerj-org/xerj/issues/2336), PR
  [#2338](https://github.com/xerj-org/xerj/pull/2338)). The address-space
  cap is crash isolation: the refusal is logged once and the worker still
  starts. Linux still applies the 1536 MiB cap. The same refusal is ignored
  for the isolated `.xls` reader. If every PDF fails because the worker
  could not start, the run finishes `ok=false exit=1
  reason=pdf-worker-unavailable` instead of looking like completed-with-junk.

- `xerj autoindex` indexes `.sql` schema, migration and routine files as
  code
  ([#2337](https://github.com/xerj-org/xerj/issues/2337), PR
  [#2342](https://github.com/xerj-org/xerj/pull/2342)). `CREATE TABLE` /
  `VIEW` / `FUNCTION` / `PROCEDURE` / `INDEX` / `TRIGGER` / `TYPE` /
  `SCHEMA` / `SEQUENCE` become symbols, and the source stays searchable. A
  comment, a quoted literal, a dollar-quoted body, or an `INSERT` / `COPY`
  nested inside another statement is not a dump. Those files used to be
  sniffed as an empty SQL dump and skipped (`no records extracted`), which
  dropped hand-written DDL. A file with a top-level `INSERT … VALUES` is
  still a row dump. A data-only `COPY … FROM stdin` dump, which the row
  parser does not read, is skipped with that reason; when the same file
  also contains schema statements, those statements are indexed as code.

- A full process memtable budget now flushes instead of leaving
  `xerj autoindex` on HTTP 429 until it gives up
  ([#2321](https://github.com/xerj-org/xerj/issues/2321), PR
  [#2325](https://github.com/xerj-org/xerj/pull/2325)). The rejection
  requests a drain (largest memtable first) rather than waiting for the
  next memory sample, and a search issued while that breaker is engaged
  publishes those memtables before it scans them. `xerj search` /
  `xerj def` saying `no indices named …` names the prefixes on the node in
  that same line, including a prefix recorded only on a catalog dataset
  document.

- The default process memory cap on a host under 16 GiB is half of that
  RAM, and segment merges follow it. This improves
  [#1122](https://github.com/xerj-org/xerj/issues/1122); the issue stays open
  (PR [#2340](https://github.com/xerj-org/xerj/pull/2340)). An 8 GiB
  machine used to take the 8 GiB step, so the budget was the whole machine;
  a merge storm on Windows 10 stayed up only after
  `XERJ_MAX_PROCESS_MEMORY_MB=4096`. That host now gets a 4 GiB cap. A
  merge batch on a budget that cannot hold the historical 512 MiB input
  (8× working set plus the 2 GiB memtable floor) is sized to `budget / 64`
  (64 MiB at 4 GiB) and concurrent merges share a quarter of the budget. A
  batch that would still cross the RSS watermark waits for the next check;
  one batch still runs, because a merge is what retires per-segment caches.
  Hosts at 16 GiB and above keep the 8 / 16 / 32 GiB steps, the 512 MiB
  batch, and the configured merge parallelism. A single JSON document over
  50 MiB is still refused at the 8 MiB ceiling, before it is parsed, and
  the rest of the bulk is indexed. The memory-ceiling class and the WSL2
  VM death are not addressed.

- A multi-term question no longer ranks a one-word symbol definition
  (`type Token`) above a hit that covers more of the question
  ([#2327](https://github.com/xerj-org/xerj/issues/2327), PR
  [#2329](https://github.com/xerj-org/xerj/pull/2329)). `xerj search`,
  `xerj def`, `xerj code` and the plain-string `xerj_search` tool add a
  coverage score when the question has three or more content tokens, and
  drop a one-term `name`+`kind` hit behind those results. A one-token
  lookup (`xerj def Token`) is unchanged. A nested test callback larger
  than a passage is no longer returned whole: the line-snapped window is
  kept, with its line. An archived path (`archive/`, `archived/`, or
  `keep_archived`) is marked on the `xerj code` header and on `xerj search`.

- Graph-mode `xerj autoindex` re-indexes a folder after files are added or
  deleted
  ([#2314](https://github.com/xerj-org/xerj/issues/2314), PR
  [#2317](https://github.com/xerj-org/xerj/pull/2317)). An added file that
  fits a frozen dataset is indexed on the rerun; a file that needs a new
  dataset stays unindexed until `--fresh`. Documents, catalog rows and
  graph edges for a file that left the folder are removed, and the run
  names that file, including when `--fresh` rebuilds the plan.
  `xerj autoindex remove` deletes that folder's resume journal and
  `sync-snapshots`, and an explicit `--state-dir` on `remove` deletes the
  journal there too. A file whose size changes while a run is reading it
  is skipped with a warning instead of aborting the run, on the graph path
  and on `--no-graph` snapshot sealing. The catalog canonical/alias count
  refreshes and recounts before it aborts a generation. A rerun that
  removes documents for files that left the folder finishes exit 3
  (`reason=files-removed`); the next unchanged rerun is exit 0.

- Scoped autoindex threads and `POST /{index}/_update/{id}` no longer abort
  the process when a thread stack or a large body cannot be allocated.
  This improves [#1122](https://github.com/xerj-org/xerj/issues/1122); the
  issue stays open (PR [#2322](https://github.com/xerj-org/xerj/pull/2322)).
  Indexing workers, sync replay, object-watch fetches and mailbox parsers
  start with `pthread_create` and do not map an alternate signal stack. A
  spawn the OS refuses is an error: the job does not run, workers that
  already started keep going, and the caller runs the work on the calling
  thread or records the failure. An update body over two 8 MiB documents
  plus 256 KiB of envelope is refused before `serde_json`, and the existing
  document is left as it was. The memory-ceiling class and the WSL2 VM
  death are not addressed.

- An idle node whose resident set sits flat at or above the memory
  watermark no longer drains every 2 seconds for the rest of the process
  ([#1981](https://github.com/xerj-org/xerj/issues/1981), PR
  [#2232](https://github.com/xerj-org/xerj/pull/2232)). The breaker releases
  once RSS has not fallen by more than 4 MiB for 2 seconds, and does not
  re-arm inside that band (page-sized `ri_phys_footprint` noise is not a
  new working set); a climb past it engages again. While the breaker stays
  engaged, drain requests double their spacing up to 15 minutes when a
  drain does not lower RSS by more than 4 MiB. `corpus index` / `autoindex`
  report `eta_quality=stalled` when a phase with a known total completes
  nothing (including 0/N), and when a phase with no total has been silent
  for 10 minutes.

- Semantic `xerj autoindex` against `--embed-mode proxy` resumes after a
  mid-run failure and does not re-embed documents already indexed, when the
  backend, configured model, endpoint and confirmed vector width are
  unchanged
  ([#2306](https://github.com/xerj-org/xerj/issues/2306), PR
  [#2307](https://github.com/xerj-org/xerj/pull/2307)). The proxy digest is
  still the model and endpoint (#533); the width is learned with one probe
  (and remembered under the data directory) and compared on resume, so a
  successful probe does not change the hash an existing index was recorded
  under. A proxy whose endpoint has not confirmed a width stays
  non-resumable, and the refusal says to make the endpoint reachable and
  rerun — already-indexed documents are kept — or to rebuild with a new
  `--state-dir` and `--prefix` only to discard the corpus. A provider that
  swaps weights without changing the name, endpoint or width is still not
  detectable.

- The node no longer aborts when a helper thread or a merge buffer cannot
  be allocated. This improves
  [#1122](https://github.com/xerj-org/xerj/issues/1122); the issue stays open
  (PR [#2315](https://github.com/xerj-org/xerj/pull/2315)). An
  in-process shard call starts its 8 MiB thread with `pthread_create` and,
  if the OS refuses, returns `could not start the shard call` without
  running the handler. `PUT /{index}/_doc/{id}` and
  `PUT /{index}/_create/{id}` refuse a body over the 8 MiB document ceiling
  before parsing it, the same way `POST /{index}/_doc` already did. A merge
  that cannot reserve its output buffer, document map, or the next survivor
  keeps the input segments and stays up. PDF and `.xls` output readers, the
  autoindex progress ticker, and the finalize-verify search use the same
  fallible spawn: a refusal fails that file, skips the ticker, or runs the
  search on the calling thread. The memory-ceiling class and the WSL2 VM
  death are not addressed.

- `POST /_aliases` stores and enforces the alias metadata it is given:
  `filter`, routing, `is_write_index` and `is_hidden` (PR
  [#2319](https://github.com/xerj-org/xerj/pull/2319), Tong-bit-art). Those
  fields were dropped by the action parser, so a filtered alias created
  through the standard API exposed every document while the same alias
  created with `PUT /{index}/_alias/{name}` worked. `_cat/aliases` printed
  `-` for the filter; it now renders `*` for a filtered alias. Removing an
  alias, or re-adding it without a filter, clears the stored metadata, so
  a stale filter cannot outlive its definition.

- `xerj corpus add --own` and `xerj corpus own <name>` (`--off` clears it)
  mark a corpus as your own first-party code (`"own": true` in its
  `corpus.json`)
  ([#2339](https://github.com/xerj-org/xerj/issues/2339), PR
  [#2343](https://github.com/xerj-org/xerj/pull/2343)). `xerj code` and MCP
  `xerj_code_search` still name an unidentified licence (`UNKNOWN` or
  `NONE-FOUND`) on each hit and omit the do-not-copy banner for that
  unidentified licence. A licence the detector actually named still warns.
  A local path is not marked own unless the flag is passed, and a `--from`
  manifest cannot set the marker. `xerj corpus index` prints a per-file-class
  indexed vs ignored-data summary at the end.

- An existing index's persisted `index.default_pipeline` runs again after a
  clean restart
  ([#2294](https://github.com/xerj-org/xerj/issues/2294), PR
  [#2296](https://github.com/xerj-org/xerj/pull/2296)). `GET /_settings` still
  reported the binding, and the pipeline definition was still there, but an
  ordinary document write no longer copied the field: the in-memory binding
  was filled on create and on settings update, and opening the index from
  disk did not rebuild it. Boot, a failed-index retry, and snapshot restore
  now copy `settings.json` back into that map. Explicit `?pipeline=` was
  already unaffected.

- A bulk reserves memory for its tokens before the documents are parsed, and
  uncompressed JSON larger than 64 MiB is streamed one value at a time. This
  improves [#1122](https://github.com/xerj-org/xerj/issues/1122); the issue
  stays open (PR [#2302](https://github.com/xerj-org/xerj/pull/2302)). A bulk
  whose analysis would cross the memory watermark is rejected with HTTP 429
  before the documents are parsed, so several in-flight bulks cannot allocate
  their token strings together. A value over the fixed 8 MiB document ceiling
  is skipped with a note that names the file, its size, and that
  `--max-file-mb` and `limits.max_process_memory_mb` do not raise that
  ceiling. A file over the scan cap names its size and `--max-file-mb`. The
  memory-ceiling class and the WSL2 VM death are not addressed.

- `POST /{index}/_refresh` makes recent writes searchable without waiting for
  a background segment merge to finish, and a multi-index or wildcard refresh
  is not serialized behind one busy index
  ([#2234](https://github.com/xerj-org/xerj/issues/2234), PR
  [#2245](https://github.com/xerj-org/xerj/pull/2245)). The route used to call
  the durability flush (snapshot fsync and WAL prune) once per index;
  autoindex finalize sent that as one `POST /<prefix>-*/_refresh` and stalled
  at the end of the run when a large matching index had a merge in progress.
  `_refresh` now publishes an unflushed memtable and returns immediately when
  the memtable is already empty. `_flush` is unchanged and still checkpoints
  the WAL. A merge encode no longer occupies a tokio blocking thread for the
  whole encode, so a refresh that does have to publish a segment is not
  queued behind that merge. Autoindex finalize refreshes each dataset index
  (and the edges index) on its own, names the index it is waiting on, and
  does not retry a timed-out refresh.

- Neural `xerj autoindex` prints an estimated embedding time before ingest,
  from a probe measured when the neural model loads and the bytes of files
  that feed a `semantic_text` dataset
  ([#1612](https://github.com/xerj-org/xerj/issues/1612), PR
  [#2250](https://github.com/xerj-org/xerj/pull/2250)). The line names no
  duration when the probe did not run, and it points at the faster lexical
  default. `--embed-mode` help and the startup warning state that the mode
  is fixed per index at ingest and that changing it requires a re-index.
  Creating or updating a mapping warns when `inference_id`, `model_id`, or
  `inference_endpoint` names a different embedder than the server is running;
  those keys do not switch the embedder.

- A `semantic` query posted at a wildcard, `_all`, an alias, or a
  comma-separated list skips an index whose mapping has no `semantic_text`
  field for that name, instead of failing the whole request with HTTP 400
  ([#2305](https://github.com/xerj-org/xerj/issues/2305), PR
  [#2308](https://github.com/xerj-org/xerj/pull/2308)). Those indices
  contribute no hits. The response stays HTTP 200, with `_shards.failed` and
  a failures entry naming the index. If every searched index is skipped, the
  request is still a 400, so a wrong field name is not an empty success. A
  query that names one concrete index is still a 400 when that index lacks
  the field. The same skip applies on `_msearch`, `_search/template`,
  `_msearch/template`, and scroll fan-out. A hybrid query whose semantic leg
  returns this error skips that index rather than failing the request.

- x86_64 release binaries no longer require AVX2
  ([#2161](https://github.com/xerj-org/xerj/issues/2161), PR
  [#2253](https://github.com/xerj-org/xerj/pull/2253)). The four x86_64
  targets pin `x86-64-v2` (SSE4.2: Nehalem / Bulldozer and later). The HNSW
  eight-accumulator dot product — the kernel `x86-64-v3` was actually
  vectorizing — is compiled again under `avx2`+`fma` and selected at runtime
  when the CPU has both features. Rust does not contract `acc += x * y`
  into FMA, so the scalar compilation and the AVX2 compilation are
  bit-identical. Pre-Nehalem (no SSE4.2) still cannot run the binary.
  Earlier in this window the installers refused a CPU without AVX2, BMI2
  and FMA and left the build at `x86-64-v3` (PR
  [#2178](https://github.com/xerj-org/xerj/pull/2178), samuelmbabhazi, from
  field report #2158). #2253 supersedes that refusal: the installers install
  on CPUs without AVX2, and `XERJ_SKIP_CPU_CHECK` is gone.

- A matching index template's `semantic_text` properties and
  `index.default_pipeline` are applied when an index is created, including
  the body `xerj autoindex` sends and the first bulk or single-document
  write that auto-creates the index. The template already matched (a
  `keyword` field from it was mapped), but `semantic_text` was stored as
  `object` and the default pipeline was dropped, so the pipeline never ran
  ([#2281](https://github.com/xerj-org/xerj/issues/2281), PR
  [#2283](https://github.com/xerj-org/xerj/pull/2283)).

- `xerj --help` lists `xerj hub` next to `xerj ask` when the build has a hub
  signing key. The `xerj ask` line names `xerj hub stop` as the way to stop
  the node that command started. A build with no hub key still does not
  advertise `xerj hub`
  ([#2280](https://github.com/xerj-org/xerj/issues/2280), PR
  [#2282](https://github.com/xerj-org/xerj/pull/2282)).

- `POST /{index}/_count` honours `min_score`. The handler rebuilds a `size: 0`
  search body from the query alone, so the threshold used to be dropped: for
  `{"query":{"match":…},"min_score":10}` `_search` returned 0 while `_count`
  returned every match. Both the single-index and the multi-index/alias paths
  now set it on the request, and a non-numeric value is the same 400
  `parse_exception` `_search` gives (PR
  [#2277](https://github.com/xerj-org/xerj/pull/2277), Tong-bit-art).

- On an autoindexed log index, a `match` query requires every token. Omitted
  tokens used to count as a hit, and a date-typed xferlog stamp is kept as a
  date ([#1504](https://github.com/xerj-org/xerj/issues/1504), PR
  [#2278](https://github.com/xerj-org/xerj/pull/2278)).

- MCP `xerj_search` cites the matched source line. Each hit can carry
  `match_line` (the absolute 1-based line of the best-scoring query term
  inside the passage) and a capped `match_lines`. Both are omitted when no
  term is located or the passage start line is unknown
  ([#2266](https://github.com/xerj-org/xerj/issues/2266), PR
  [#2269](https://github.com/xerj-org/xerj/pull/2269)).

- MCP `xerj_code_search` token budgets preserve adjacent definitions and
  distinct snippets at the same citation. Rendered text is deduped only when
  both its locator and displayed passage match an already-shown block;
  display labels, separators and match citations no longer stand in for
  source ranges (PR [#2272](https://github.com/xerj-org/xerj/pull/2272),
  Jarvis636431).

- MCP JSON search responses with `max_tokens` no longer deduplicate rows
  with distinct `ax_locator` values or hits from different indices merely
  because their relative paths and passage line numbers match. Deduplication
  includes the available source index, file identity and record locator;
  overlapping file and code-symbol passages in the same source still
  deduplicate. Plain-string `xerj_search` requests the provenance fields
  needed for this distinction by default (PR
  [#2255](https://github.com/xerj-org/xerj/pull/2255), Jarvis636431).

- `xerj search --prefix`, `xerj def --prefix`, and MCP `xerj_search`'s
  `prefix` argument resolve through one function: the prefix `xerj autoindex`
  stores. `My_Proj` searches `my-proj-*`, and a trailing `-*` is not part of
  the prefix. A prefix that cannot name an index is refused before any
  request. Omitting both `index` and `prefix` on `xerj_search` still searches
  `ax-*` (PR [#2235](https://github.com/xerj-org/xerj/pull/2235); #2080).

- SIGTERM and SIGINT keep the HTTP ports bound until the final memtable
  flush has finished writing the data directory. New HTTP requests during
  that window get 503, except `/health/live`, which stays 200 so a liveness
  probe does not restart the process mid-flush. Connection refusal is the
  signal that the process is done with the data directory; closing the port
  first let a wipe that keys off that signal delete a segment the flush was
  still publishing (PR [#2246](https://github.com/xerj-org/xerj/pull/2246)).

- A `node.lock` left by a process that is no longer running no longer blocks
  the next start
  ([#2233](https://github.com/xerj-org/xerj/issues/2233), PR
  [#2246](https://github.com/xerj-org/xerj/pull/2246)). The data-directory
  lock is still the OS file lock, so a live node blocks a second start. When
  that lock is busy and the PID recorded in the file is not running — a
  Windows crash can leave `LockFileEx` stuck after the process is gone — the
  file is replaced and the reclaim is logged. A PID that cannot be read or
  checked is left alone. `--auto-port` does not treat that stale lock as a
  reason to start on an empty sibling directory.

- `xerj corpus index` killed mid-index resumes that build's journal instead of
  starting a legacy index under a new prefix. When the journal still has a
  generation in progress, indexing continues at the uncommitted operations and
  files already applied are not hashed again. A resume that verifies records
  the build. A resume that cannot verify is recorded as unverified, or left
  unrecorded with the journal kept, instead of a ledger that names no build
  ([#2239](https://github.com/xerj-org/xerj/issues/2239), PR
  [#2243](https://github.com/xerj-org/xerj/pull/2243)).

- `xerj corpus index` drains when the memtable is under pressure, so a corpus
  that used to sit in a breaker loop (writes 429, nothing indexed) makes
  progress ([#2238](https://github.com/xerj-org/xerj/issues/2238), PR
  [#2244](https://github.com/xerj-org/xerj/pull/2244)).

- `_validate/query` now asks every resolved index the same buildability
  question `_search` does: a query naming a field that is neither indexed nor
  has doc values, or a non-numeric value against a numeric field, reports
  `valid: false` (with `error`/`explanations`) instead of claiming a body
  `_search` rejects is valid. The parse-only answer is kept for a selector
  that resolves to no index
  ([#1284](https://github.com/xerj-org/xerj/issues/1284),
  [#204](https://github.com/xerj-org/xerj/issues/204), PR
  [#2263](https://github.com/xerj-org/xerj/pull/2263), Tong-bit-art).

- A `semantic` query on a `semantic_text` field returns `highlight` fragments
  when the request asks for them
  ([#1947](https://github.com/xerj-org/xerj/issues/1947), PR
  [#2227](https://github.com/xerj-org/xerj/pull/2227)). The fragments are the
  field's best-scoring chunks for the query embedding, in score order, limited
  by `number_of_fragments` (default 5; `0` returns the whole field). `order:
  none` emits that same selection in document order. `"type": "semantic"`
  selects this highlighter, and it is the default for a `semantic_text`
  field on a semantic query. Chunks are not wrapped in `<em>` tags. A
  lexical `match` on the same field still highlights query terms.

- A single JSON document over 8 MiB is not indexed, so one file of 50 MB
  or more cannot abort the node during autoindex or `_bulk`. This is a
  partial fix; [#1122](https://github.com/xerj-org/xerj/issues/1122) stays
  open (PR [#2256](https://github.com/xerj-org/xerj/pull/2256); the
  helper-thread half is PR
  [#2226](https://github.com/xerj-org/xerj/pull/2226); the Windows
  commit-space half is PR
  [#2236](https://github.com/xerj-org/xerj/pull/2236)). Autoindex
  streams a top-level array, or a top-level array of records inside a larger
  object, one element at a time and skips only the values that exceed the
  limit. The skip names the file. A bulk item over the limit is HTTP 413 and
  its neighbours still index. The ceiling is fixed, not a new setting. This
  pull request still did not read a file over 64 MiB; the skip named that
  cap. PR [#2302](https://github.com/xerj-org/xerj/pull/2302), above, streams
  uncompressed JSON past it. #1122 stays open.

- Opening an index whose snapshot names a segment file that is already
  gone drops that segment instead of failing every later `_search` with
  `Segment … not found`. A sibling segment that is still on disk stays
  readable, and sequence numbers from the missing segment are not reused.
  Object-store mode is unchanged: a catalogue whose bundle is missing
  still refuses to open. This is what let `xerj brain --fresh` finish
  its catalog read-back after a data directory was only partly removed
  (PR [#2256](https://github.com/xerj-org/xerj/pull/2256)).

- A best-effort helper thread that cannot be created no longer aborts the
  process. Partial fix of
  [#1122](https://github.com/xerj-org/xerj/issues/1122); the issue stays
  open (PR [#2226](https://github.com/xerj-org/xerj/pull/2226)). On Unix
  those threads are started with `pthread_create` and do not map an
  alternate signal stack, so the allocation failure that used to panic
  inside the child (`failed to allocate an alternative stack`, os error 12)
  comes back as an error. Drain-free drops the generation on the caller; pool
  work runs inline; WAL fsync, background merge, and the ingest-memory trace
  skip the thread and retry or stay off; share-tunnel and ONNX init fail that
  startup step. Each site logs once. `panic = "abort"` is unchanged.

- On Windows, the process memory cap follows commit space, not only physical
  RAM. The AUTO tier is still chosen from physical RAM, then lowered to
  `process commit + available commit` minus a reserve (one quarter of that
  ceiling, clamped to 512 MiB..2 GiB). The sampler compares the max of
  working set and commit charge with the watermark, and engages the breaker
  when available commit falls under the reserve. The HTTP 429 names the
  commit limit and how to enlarge the pagefile. Raising
  `indices.breaker.total.limit` or `XERJ_MAX_PROCESS_MEMORY_MB` cannot grant
  commit Windows will not give. Linux and macOS budgets are unchanged. This
  is a partial fix; [#1122](https://github.com/xerj-org/xerj/issues/1122)
  stays open: the memory-ceiling class and the WSL2 VM death are not
  addressed (PR [#2236](https://github.com/xerj-org/xerj/pull/2236), from
  field report #2174).

- A flood-stage write block is explained in plain text. `POST /_memory/{ns}`,
  `xerj memory store` and MCP `xerj_memory_store` say the disk holding the
  data dir is nearly full, with free space, used percent, the threshold, the
  data-dir path, and that freeing space or restarting with `--data-dir` on
  another drive is the recovery. The previous error was only a nested
  `cluster_block_exception`. An explicit `index.blocks.write`, and a manual
  `read_only_allow_delete` that does not mention flood-stage, stay the
  original error
  ([#2284](https://github.com/xerj-org/xerj/issues/2284), PR
  [#2290](https://github.com/xerj-org/xerj/pull/2290)).

- `xerj corpus add <name> <local-path>` copies the checkout's root
  `.xerjignore` into the clone when that file is untracked or has uncommitted
  edits. `git clone` only checks out committed files, so the ignore file was
  dropped and the index used built-in rules only
  ([#2199](https://github.com/xerj-org/xerj/issues/2199), PR
  [#2204](https://github.com/xerj-org/xerj/pull/2204)). A pinned `--from`
  rebuild stays at the recorded SHA and does not copy it; it says so when the
  working tree's root `.xerjignore` differs. Nested `.xerjignore` files are
  not carried.

- A repo pin harvests the paths the pin names, not the whole tree
  ([#1842](https://github.com/xerj-org/xerj/issues/1842), PR
  [#2228](https://github.com/xerj-org/xerj/pull/2228)).

- `xerj autoindex` no longer wedges or runs out of memory on a JSON object
  made of several top-level arrays of records
  ([#1856](https://github.com/xerj-org/xerj/issues/1856),
  [#1978](https://github.com/xerj-org/xerj/issues/1978), PR
  [#2131](https://github.com/xerj-org/xerj/pull/2131)). When the largest array
  is longer than a config document (32 records), each top-level array of
  objects becomes its own dataset instead of being copied onto every row of
  the largest one. A shared value over 4 KiB is stored once. A data column
  that would be `semantic_text` falls back to plain text — still searchable —
  when the real record count exceeds 100,000 or the elected text exceeds 8
  MiB, and the plan says so, including `--no-semantic`. The pre-index budget
  counts records in `.json` files from 256 KiB up to 64 MiB and can refuse on
  a structural memory ceiling. That ceiling is not a measured store ratio, and
  no new disk ratio was invented for `.json`.

- Semantic embeddings for a JSONL autoindex are packed so resident memory
  stays bounded. One small-record JSONL used to cost on the order of a
  couple of hundred kilobytes of node RSS per record
  ([#1940](https://github.com/xerj-org/xerj/issues/1940), PR
  [#2240](https://github.com/xerj-org/xerj/pull/2240)).

- `exists` on a text field is answered without hydrating the stored corpus
  (PR [#2130](https://github.com/xerj-org/xerj/pull/2130)).

- An embedding width that contradicts the `semantic_text` mapping is a
  per-item 400, not a retryable 500
  ([#2191](https://github.com/xerj-org/xerj/issues/2191), PR
  [#2217](https://github.com/xerj-org/xerj/pull/2217)).

- Case-aliased field sidecars no longer abort publish
  ([#2254](https://github.com/xerj-org/xerj/issues/2254), PR
  [#2261](https://github.com/xerj-org/xerj/pull/2261)). A side-car build or
  publish that fails now reports its cause (PR
  [#2260](https://github.com/xerj-org/xerj/pull/2260), samuelmbabhazi).

- `POST /{index}/_refresh` and autoindex `finalize-refresh` no longer wait
  forever when a shard flush cannot get a finalize-gate or flush-semaphore
  permit. Admission gives up after `engine.flush_admit_timeout_secs`
  (default 120 seconds, override `XERJ_FLUSH_ADMIT_TIMEOUT_SECS`) and does
  not freeze the memtable. The caller gives up after
  `engine.flush_timeout_secs` (default 300 seconds, override
  `XERJ_FLUSH_TIMEOUT_SECS`) and leaves a publication that has already
  frozen its memtable running. The refresh response names the stuck index
  under `_shards.failures` and continues with the other indices. Autoindex
  reports that failure instead of treating the refresh as success
  ([#1635](https://github.com/xerj-org/xerj/issues/1635), PR
  [#2129](https://github.com/xerj-org/xerj/pull/2129)). Flush deadlines are
  measured on the wall clock, not a tokio clock that a test can pause (PR
  [#2167](https://github.com/xerj-org/xerj/pull/2167), same issue).

- `_cat/indices` and `_cat/shards` use the bare content count, so
  `docs.count` agrees with `_count` and no longer includes file-card anchors
  (PR [#2141](https://github.com/xerj-org/xerj/pull/2141), Tong-bit-art).
  `docs.deleted` on `_cat` and the stats surfaces is the real deleted count
  (PR [#2242](https://github.com/xerj-org/xerj/pull/2242), Tong-bit-art).
  Both close [#2140](https://github.com/xerj-org/xerj/issues/2140).

- `xerj index --file <dir>` is refused before any index state is written,
  instead of dying in `mmap` with `Invalid argument` after creating an empty
  index ([#1818](https://github.com/xerj-org/xerj/issues/1818), PR
  [#2172](https://github.com/xerj-org/xerj/pull/2172), elewashy).

- Autoindex and corpus, the rest of this window:
  - A dead node is reported in full, and `_bulk` retries stop
    ([#2177](https://github.com/xerj-org/xerj/issues/2177), PR
    [#2192](https://github.com/xerj-org/xerj/pull/2192)).
  - A watched file can grow a field without failing the pass
    ([#2176](https://github.com/xerj-org/xerj/issues/2176), PR
    [#2193](https://github.com/xerj-org/xerj/pull/2193)).
  - Replacing a file drops the previous symbol generation
    ([#2139](https://github.com/xerj-org/xerj/issues/2139), PR
    [#2197](https://github.com/xerj-org/xerj/pull/2197)).
  - `xerj autoindex --watch` drops events on ignored files before they start
    a pass ([#2180](https://github.com/xerj-org/xerj/issues/2180), PR
    [#2198](https://github.com/xerj-org/xerj/pull/2198), samuelmbabhazi).
  - A changed file in a field-capped dataset is kept
    ([#1815](https://github.com/xerj-org/xerj/issues/1815), PR
    [#2215](https://github.com/xerj-org/xerj/pull/2215)).
  - `--lang` accepts a file extension and stores the language name
    ([#2205](https://github.com/xerj-org/xerj/issues/2205), PR
    [#2219](https://github.com/xerj-org/xerj/pull/2219)).
  - A dataless macOS placeholder is skipped instead of blocking the walk
    ([#2058](https://github.com/xerj-org/xerj/issues/2058), PR
    [#2223](https://github.com/xerj-org/xerj/pull/2223)).
  - A graph-path run whose catalog rows are not visible fails
    ([#1977](https://github.com/xerj-org/xerj/issues/1977), PR
    [#2224](https://github.com/xerj-org/xerj/pull/2224)).
  - `finalize-verify` stops when its target index is deleted
    ([#2222](https://github.com/xerj-org/xerj/issues/2222), PR
    [#2225](https://github.com/xerj-org/xerj/pull/2225)).
  - A resumed catalog read-back stays on the changed set. Same-generation
    resume confirms the generation with one `size: 0` count instead of
    paging every catalog document
    ([#1267](https://github.com/xerj-org/xerj/issues/1267), PR
    [#2231](https://github.com/xerj-org/xerj/pull/2231)).
  - `finalize-verify` can capture between catalog writes
    ([#1761](https://github.com/xerj-org/xerj/issues/1761), PR
    [#2237](https://github.com/xerj-org/xerj/pull/2237)).
  - Catalog phrase text is not indexed as a code symbol, and a phone-format
    syslog line parses (PR
    [#2247](https://github.com/xerj-org/xerj/pull/2247)).
  - An unreadable corpus source fails the build instead of pruning every
    stored envelope for that slug. Empty ids, and a torn final record whose
    checksums were recomputed, are refused
    ([#1110](https://github.com/xerj-org/xerj/issues/1110), PR
    [#2258](https://github.com/xerj-org/xerj/pull/2258)).

- `xerj code` and Corpus Hub, the rest of this window:
  - Lower-case `license.txt` and `License.md` are detected
    ([#2201](https://github.com/xerj-org/xerj/issues/2201), PR
    [#2202](https://github.com/xerj-org/xerj/pull/2202)).
  - Python passages cite a real start line, from symbols or the checkout
    ([#2195](https://github.com/xerj-org/xerj/issues/2195), PR
    [#2203](https://github.com/xerj-org/xerj/pull/2203)).
  - A weak nearest-neighbour page is labeled
    ([#1986](https://github.com/xerj-org/xerj/issues/1986), PR
    [#2221](https://github.com/xerj-org/xerj/pull/2221)).
  - An empty corpus ledger names the autoindex prefixes that are actually
    indexed ([#2181](https://github.com/xerj-org/xerj/issues/2181), PR
    [#2230](https://github.com/xerj-org/xerj/pull/2230)).
  - `xerj hub find` prints `slug — owner/name — description` when the
    catalogue has `upstream`. This binary writes catalogue format 2 and
    still reads format 1, where that field is absent and prints empty.
    The published catalogue is still format 1, so owner/name is empty
    until a binary with this code rebuilds it. rc.98 and older clients
    refuse a format other than 1 (PR
    [#2249](https://github.com/xerj-org/xerj/pull/2249)).
  - A question whose topic no hub corpus covers is `NoCoverage`, not an
    ambiguous tie. `xerj ask` prints that no corpus covers the topic, points
    at `xerj autoindex` and `xerj hub find`, and exits 1. Exit 4 is unchanged
    when several corpora actually match. This is a partial fix of
    [#2179](https://github.com/xerj-org/xerj/issues/2179): the issue stays
    open, because no Phaser, pandas or charting pack was added and a slug
    that names a different project is still not explained (PR
    [#2267](https://github.com/xerj-org/xerj/pull/2267)).

### Testing

- A dropped replica is refilled to the primary's live set before it
  rejoins: insert, update and delete during the outage, then reopen the
  same data directory. Test only; no production change. It exercises the
  backfill in PR [#2328](https://github.com/xerj-org/xerj/pull/2328). Part
  of [#1170](https://github.com/xerj-org/xerj/issues/1170); the issue stays
  open (PR [#2341](https://github.com/xerj-org/xerj/pull/2341)).
- The MCP tool-count assertion was updated in the same change as local
  definition lookup (PR [#2166](https://github.com/xerj-org/xerj/pull/2166)).

### Documentation & site

- `/product` was rewritten against the v1.0.0-rc.98 binary (PR
  [#1926](https://github.com/xerj-org/xerj/pull/1926)). Eight answer pages
  from the 2026-10-20 agent-query captures (PR
  [#2251](https://github.com/xerj-org/xerj/pull/2251)). The Day/Night choice
  is kept across pages
  ([#1973](https://github.com/xerj-org/xerj/issues/1973), PR
  [#2169](https://github.com/xerj-org/xerj/pull/2169), samuelmbabhazi).
  Homepage `Link` headers advertise the API documentation (PR
  [#2291](https://github.com/xerj-org/xerj/pull/2291), mariomarcolongo).
  The Cline row in the English, Korean and Simplified Chinese README MCP
  tables is marked tested. Cline CLI 3.0.73 on Linux loaded the README
  `mcpServers` block against `xerj` v1.0.0-rc.98; `xerj_search` was in the
  tool list and one call ran. The IDE Configure button was not part of that
  run (PR [#2300](https://github.com/xerj-org/xerj/pull/2300)).
  The primary nav's plain GITHUB link is a stars badge: the GitHub mark and
  the star count, linking to the repository. The shipped count is 14,225,
  read 2026-10-24T02:05:55Z. `theme.js` refreshes it from the GitHub API,
  cached in the browser for 10 minutes, and keeps the shipped number if
  that call fails (PR [#2311](https://github.com/xerj-org/xerj/pull/2311)).
  The site states the MCP tool count main registers: 22 with a hub signing
  key (`xerj_def` and `xerj_batch_search` included), 19 when the three
  `xerj_hub_*` tools are dropped. The published `mcp-tools.json` already
  lists 22. The v1.0.0-rc.98 figure stays 20 and now names both tools that
  binary lacks (PR [#2350](https://github.com/xerj-org/xerj/pull/2350)).
  Canonical description, synced figures, FAQPage JSON-LD, `llms.txt`,
  an IndexNow key, and MCP listing manifests (`server.json` and the
  mcpb manifest). `.claude-plugin/marketplace.json` is a Claude Code
  plugin marketplace named `xerj` (plugin source `./plugins/xerj`).
  `packaging/docker-mcp/Dockerfile` builds the Docker MCP image
  `xerj-mcp` (stdio `xerj mcp`; the file's default `XERJ_VERSION` is
  `1.0.0-rc.98`). `landing/robots.txt` adds
  `Content-Signal: search=yes, ai-input=yes, ai-train=yes`. The
  registry manifest names the tagged release v1.0.0-rc.98 (PR
  [#2323](https://github.com/xerj-org/xerj/pull/2323)). The mcpb package
  identifier points at the prerelease asset
  `releases/download/mcpb-v1.0.0-rc.98/xerj-mcp-1.0.0-rc.98.mcpb`, not
  `releases/download/v1.0.0-rc.98/` (PR
  [#2364](https://github.com/xerj-org/xerj/pull/2364)).
- A ready-to-paste DeepSeek Harness MCP config, with `env` and `cwd`
  ([#2286](https://github.com/xerj-org/xerj/issues/2286), PR
  [#2288](https://github.com/xerj-org/xerj/pull/2288)). Both Windows install
  directories are named: `~/.local/bin` for the `sh` installer, including
  Git Bash, MSYS and Cygwin, and `%LOCALAPPDATA%\Programs\xerj` for
  PowerShell
  ([#2285](https://github.com/xerj-org/xerj/issues/2285), PR
  [#2289](https://github.com/xerj-org/xerj/pull/2289)).
- AGENTS.md listed twenty MCP tools at the time (PR
  [#2150](https://github.com/xerj-org/xerj/pull/2150), redwolf2019).
  After PR [#2166](https://github.com/xerj-org/xerj/pull/2166)
  (`xerj_def`) and PR [#2333](https://github.com/xerj-org/xerj/pull/2333)
  (`xerj_batch_search`) the count main registers is 22, or 19 when the
  three `xerj_hub_*` tools are dropped. The
  Simplified Chinese README matches the English one (PR
  [#2149](https://github.com/xerj-org/xerj/pull/2149), redwolf2019). A Korean
  README translation (PR [#2164](https://github.com/xerj-org/xerj/pull/2164),
  pallidev).

### Community & field reports

- Field reports: multi-array JSON autoindex (PR
  [#2132](https://github.com/xerj-org/xerj/pull/2132)); MCP prefix isolation
  (PR [#2138](https://github.com/xerj-org/xerj/pull/2138), Jarvis636431);
  Windows 16 GB, three autoindex aborts, WAL replay restores the corpora (PR
  [#2174](https://github.com/xerj-org/xerj/pull/2174), WYSTAF); the #1940
  JSONL RSS reproduction (PR
  [#2241](https://github.com/xerj-org/xerj/pull/2241)); an rc.98 `xerj ask`
  of lz4 (PR [#2252](https://github.com/xerj-org/xerj/pull/2252)); consuming
  the published rust-vulns pack (PR
  [#2259](https://github.com/xerj-org/xerj/pull/2259)); MCP budget identity
  (PR [#2265](https://github.com/xerj-org/xerj/pull/2265), Jarvis636431);
  `corpus add` plus a hub-manifest pull request (PR
  [#2270](https://github.com/xerj-org/xerj/pull/2270), kamwoh); a Cline MCP
  setup from the README block, CLI only (PR
  [#2301](https://github.com/xerj-org/xerj/pull/2301)); batch retrieval
  against a fresh isolated node with synthetic advisories — usage only,
  not a quality or performance evaluation (PR
  [#2334](https://github.com/xerj-org/xerj/pull/2334), Jarvis636431, written
  with OpenAI Codex).
- CLA signatures: ArkNill (PR
  [#2153](https://github.com/xerj-org/xerj/pull/2153)), pallidev (PR
  [#2165](https://github.com/xerj-org/xerj/pull/2165)), samuelmbabhazi (PR
  [#2168](https://github.com/xerj-org/xerj/pull/2168)), elewashy (PR
  [#2173](https://github.com/xerj-org/xerj/pull/2173)), WYSTAF (PR
  [#2175](https://github.com/xerj-org/xerj/pull/2175)), kamwoh (PR
  [#2264](https://github.com/xerj-org/xerj/pull/2264)), mgtf (PR
  [#2275](https://github.com/xerj-org/xerj/pull/2275)), mariomarcolongo
  (PR [#2292](https://github.com/xerj-org/xerj/pull/2292)). Jun516 joined
  the contributor list (PR
  [#2335](https://github.com/xerj-org/xerj/pull/2335)).

### Housekeeping

- Release-download counters landed as `[skip ci]` chores, not pull requests:
  2026-10-22 (30,604 assets, 25,047 binaries), 2026-10-23 (31,411 assets,
  25,591 binaries) and 2026-10-24 (33,978 assets, 27,993 binaries). The
  figures are the commit subjects.


## [1.0.0-rc.98] - 2026-10-21

Window since v1.0.0-rc.97: a console-API security fix (CVE-2026-97406), a
budget-aware `xerj autoindex` that estimates the index and checks it against
the node's flood-stage watermark before hashing (with `--plan`, `--yes` and
`--max-index-size`; a reduced plan never deletes already-indexed data without
`--drop-indexed`), `xerj autoindex remove` for dropping one folder safely,
`--walk-only`, a fail-fast on a full node disk, ES
request validation and query/aggregation fixes, highlighting, search ranking
and citation fixes, Corpus Hub routing and ratings, and the field-report
footer honouring the opt-out. Contributors in this window: amartya1523,
Tong-bit-art, roedyrustam, jacksonjp0311-gif, MrUranium1, jayhemnani9910,
leninworld, gyanu2507, dhkts1, Jarvis636431, ravi-arnan, rudycelekli, AK-Lmn,
miku1130, tree-lancer, Zacharon, tizerluo, olituks,
tripleaceme, MokuLakshithReddy, HiChat-fog, eltociear, omertahaoztop,
ronitgupta138 and Jitin2102.


### Security

- **Console API: a bearer API token can no longer mint credentials**
  (CVE-2026-97406,
  [CERT VU#645818](https://kb.cert.org/vuls/id/645818), published 2026-10-20).
  `POST /_xerj-console/api/v1/auth/api-tokens` accepted any `AuthSession`, and
  a passkey cookie and `Authorization: Bearer <api token>` both resolved to
  that type. Holding one API token was enough to mint independent successors
  — revoking the parent left them working — and an owner token could issue
  an invitation, which the attacker redeemed in their own browser to enroll
  their own passkey. Creating and revoking API tokens, issuing invitations,
  deleting passkeys, and logout now require a passkey-backed browser session
  and return 403 for a bearer token. Ordinary console calls (`/me`, prefs,
  dashboards, and the other data routes) still accept bearer tokens. No
  token lineage is stored: the only insert of a new API token row refuses
  bearer auth before it writes, so a revoked token has no descendants (PR
  [#2003](https://github.com/xerj-org/xerj/pull/2003)).

### Added

- `xerj autoindex remove --prefix P [--root R]` and the MCP tool
  `xerj_autoindex_remove` drop one autoindexed folder. They list the indices,
  catalog rows, doc counts and sizes first and delete nothing without `--yes`,
  a `y` at the terminal, or `confirm: true`. An index another folder or a
  sibling prefix still names is kept, and `autoindex-catalog` is never
  deleted. `DELETE` of `*`, `_all` or a wildcard is now refused unless
  `action.destructive_requires_name` is set to false, as in ES 8. The agent
  docs and tool descriptions say not to delete an index the agent did not
  create (PR [#2126](https://github.com/xerj-org/xerj/pull/2126), from field
  report #2114).
- `GET /_memory` lists the agent-memory namespaces (PR
  [#2022](https://github.com/xerj-org/xerj/pull/2022), ravi-arnan).
- `xerj autoindex --walk-only` stops after the walk. It prints each top-level
  folder's file count and size, what the ignore rules would drop and which
  rule, and the totals. It does not hash or sniff file contents. `--dry-run`
  still hashes, sniffs and prints the plan (#1912).

### Fixed

- `xerj autoindex` no longer deletes an existing index when a later run uses
  a reduced disk-budget plan (`--plan code-and-docs`, or `--yes` when the
  default plan does not fit). The plan's keep-set was applied to the walk
  before reconcile, so files the plan left out looked removed: the
  `--no-graph` generation emitted `Delete` for their groups, and the graph
  path swept them as exclusions. A re-run that would drop already-indexed
  files now exits 4 and lists those files and the datasets that would be
  left empty. `--drop-indexed` is the confirmation that deletes them.
  `--plan` and `--yes` choose what to index; they do not agree to the
  deletion. The regression came from #2102 and never shipped in a release
  (PR [#2128](https://github.com/xerj-org/xerj/pull/2128)).

- `POST /_ask` (`xerj_plan`) grounds a slash-bearing path in the prompt as a
  keyword `prefix` on `ax_path` (then `path`, `filepath`, `file`, `ax_file`).
  A directory gets a trailing slash so `crates/app/src/` does not match
  `crates/app/src2/`. An index with none of those keyword fields is a 422
  that names the path, instead of dropping it as an ignored lowercase
  modifier. `xerj_map` lists the `ax_*` locators stamped on every record,
  and when the live mapping can be read its `field_count` is the catalog
  fields union the top-level mapping fields — the same set the plan counts,
  labeled on both sides. Cardinality of a locator the profiler never sampled
  stays unmeasured. Two slash-joined words such as `tcp/ip` stay prose
  ([#2120](https://github.com/xerj-org/xerj/issues/2120), PR
  [#2124](https://github.com/xerj-org/xerj/pull/2124)).

- A bare hub question that names a language or a technology
  (`xerj "how do I use async let in Swift"`, `xerj "Swift Result type map"`)
  routes from the catalogue entry — slug, topics, a language word in the
  description, then the rest of the description — instead of the term sketch.
  On the 468-corpus hub the sketch's false positives tied swift-book with
  unrelated corpora (qwik, docker-docs, harper, oss-review-toolkit) and both
  questions exited 4. One clear winner is searched on its own. Two or three
  corpora that share the winning name are searched together and named on one
  `using:` line. Exit 4 stays for a genuine tie and lists at most five
  candidates, best first.

- An MCP tool call that cannot connect to `XERJ_URL` now says that no XERJ
  node is listening there, that `xerj mcp` does not start one, and prints
  `xerj --insecure --data-dir ~/xerj-data`. A bare connection error was the
  whole tool result, so agents abandoned xerj for the session. HTTP
  responses, including 401, are unchanged (#2079).

- `xerj autoindex` on the graph path no longer counts files that appeared
  after the resume plan was frozen as junk. A resume whose plan predates
  files now on disk still cannot re-cluster that plan — dataset identity and
  graph anchors stay put, and the `--no-graph` route is what reconciles
  additions. The resume finishes `reason=stale-resume-plan` (exit 3, recorded,
  not a failure), leaves `junk_files` for unusable input, sets
  `skipped_appeared` to the count on every resume while those files are
  unindexed, and names `--fresh` as the flag that rebuilds the plan in place
  and includes them
  ([#2076](https://github.com/xerj-org/xerj/issues/2076)).

- `xerj autoindex` checks the node's data volume against the disk
  flood-stage watermark before walking or hashing. A volume already at or
  over the watermark (default 95%) used to fail only at the first write,
  after the prepare. The run now stops immediately and reports the free
  space, the threshold, and `limits.disk_flood_stage_percent`. The probe
  reads the node (`GET /_cat/allocation?format=json`), not the client's
  disk. A node that does not report the threshold is unchanged, and
  `--dry-run` (which writes nothing) is never refused for disk.

- `xerj autoindex` counts the walked folder and estimates the index before
  hashing. The free-space budget reuses that same allocation row (free bytes
  under the reported flood-stage watermark; the threshold is not guessed),
  and `--max-index-size` can lower it. When the default plan does not fit,
  stdout is a JSON plan list and the run exits 4 until `--yes` takes the
  recommended plan that fits or `--plan` names one; a named plan runs even over
  the estimate, with a warning, since the estimate errs high. A kind with no measured ratio stays unmeasured and
  does not refuse the run. A digit-leading `--prefix` is rejected before the
  walk ([#2057](https://github.com/xerj-org/xerj/issues/2057)). Skipped files
  are named after the `ok:` line, and a PDF reuse-fallback line says the PDF
  is still indexed ([#2056](https://github.com/xerj-org/xerj/issues/2056)).
  A folder already indexed under another prefix is named before the walk
  ([#2078](https://github.com/xerj-org/xerj/issues/2078)). MCP:
  `xerj_index_budget` returns the same estimate and does not index.

- Playground user dashboards resolve their inherited template for chart data
  and panel editing. Cloning a user dashboard retains the underlying template;
  missing or cyclic template references keep the blank fallback. Custom
  dashboard layout overrides also survive reloading the page.

- Landing-page copy buttons report `COPY FAILED` when clipboard access is
  denied or the legacy copy command fails, and retain their original label
  after repeated clicks. A failed copy can be retried.

- Playground PNG exports preserve computed SVG paint and text styles, including
  the embedding query crosshair whose CSS-variable stroke disappeared when
  rasterized outside the page stylesheet. The live chart is left unchanged.

- Release-download snapshots now refuse malformed existing JSONL history without
  replacing its contents. Repair the history file and rerun the snapshot command.

- Playground saved views immediately restore the current dashboard, replace
  stale URL filters when navigating, and preserve custom time-range endpoints.

- Debian release-candidate package versions now sort before the corresponding
  final release, preserving normal upgrade order without renaming release assets.

- Debian Rust coverage reports now include valid zero-overlap measurements instead
  of exiting before printing counts. Empty source indexes remain an error.

- Hub pack planning reports malformed manifest JSON shapes as per-corpus warnings
  and continues planning healthy corpora, rather than crashing on an invalid entry.

- Hub re-pin automation only supersedes generated proposal branches for the
  exact corpus, preserving proposals for corpora with longer overlapping names.

- `xerj search` and a plain-string `xerj_search` rank a file that contains
  the query as an exact phrase (a pasted error message or log line) at the
  top. The bag-of-words leg alone let a short `defs` list sharing two of the
  words outrank the long file holding all of them in order. On 100 exact
  string literals from XERJ's own sources the defining file went from rank 1
  on 16 to rank 1 on 91, with symbol lookups unchanged
  ([harness](benchmarks/search-exact-phrase/README.md)).

- `xerj autoindex --help` now states the existing `--bulk-mb` maximum of 24.

- ES API compatibility, validated like ES with ES's own messages:
  aggregation parameters and placements (PR
  [#1861](https://github.com/xerj-org/xerj/pull/1861)), composite aggregation
  definitions (PR [#1854](https://github.com/xerj-org/xerj/pull/1854)),
  misplaced pipeline aggregations and bad `buckets_path`s (PR
  [#1824](https://github.com/xerj-org/xerj/pull/1824)), index-creation,
  mapping, alias and template requests (PR
  [#1870](https://github.com/xerj-org/xerj/pull/1870)), and the per-index
  search request limits (PR [#1859](https://github.com/xerj-org/xerj/pull/1859)),
  all amartya1523. A bad index name is an `invalid_index_name_exception` (PR
  [#1666](https://github.com/xerj-org/xerj/pull/1666)). A failed `_bulk` item
  omits versioning fields (PR [#1672](https://github.com/xerj-org/xerj/pull/1672)).
  `_cat/indices` hides `.xerj_*` rows by default (PR
  [#1660](https://github.com/xerj-org/xerj/pull/1660)), and 404 index hints
  stop suggesting system indices (PR
  [#1661](https://github.com/xerj-org/xerj/pull/1661)). A mid-name wildcard
  matches in index patterns (PR [#1937](https://github.com/xerj-org/xerj/pull/1937))
  and `_msearch` expands wildcard indices (PR
  [#1939](https://github.com/xerj-org/xerj/pull/1939)).
- Queries and aggregations: range queries honour `time_zone` (PR
  [#1744](https://github.com/xerj-org/xerj/pull/1744), jayhemnani9910). A range
  matches every value of a multi-valued field (PR
  [#1776](https://github.com/xerj-org/xerj/pull/1776), amartya1523), and range
  aggregations bucket every value (PR
  [#1803](https://github.com/xerj-org/xerj/pull/1803), Tong-bit-art).
  `case_insensitive` works on prefix and regexp queries (PR
  [#1816](https://github.com/xerj-org/xerj/pull/1816), MrUranium1).
  `filter`/`filters` aggregations count what the same clause counts as a query
  (PR [#1752](https://github.com/xerj-org/xerj/pull/1752), Tong-bit-art).
  Histogram data buckets outside `extended_bounds` are kept (PR
  [#1823](https://github.com/xerj-org/xerj/pull/1823), amartya1523).
- Highlighting: whole words and every query term in a fragment are tagged (PR
  [#1949](https://github.com/xerj-org/xerj/pull/1949)), `highlight.fields` is
  accepted as an array of field objects (PR
  [#1950](https://github.com/xerj-org/xerj/pull/1950)), and `_msearch` items
  carry highlight like `_search` (PR [#1952](https://github.com/xerj-org/xerj/pull/1952)).
- Search: a file that holds the exact query phrase ranks first (PR
  [#1759](https://github.com/xerj-org/xerj/pull/1759), jacksonjp0311-gif). A data
  row is cited by its locator, not by a line inside the row (PR
  [#1944](https://github.com/xerj-org/xerj/pull/1944)). `xerj code` cites real
  file lines for prose and Markdown hits (PR
  [#1908](https://github.com/xerj-org/xerj/pull/1908)), and prose sections carry
  the file line they start on (PR [#2013](https://github.com/xerj-org/xerj/pull/2013),
  Tong-bit-art). A plain-string `xerj_search` reads the target mapping first
  (PR [#1976](https://github.com/xerj-org/xerj/pull/1976), Tong-bit-art). A
  plain-string search over a mixed index pattern no longer reports a field as
  unknown when another matched index maps it (PR
  [#2123](https://github.com/xerj-org/xerj/pull/2123)). On a code corpus,
  plain-word `xerj search` and `xerj_search` apply `xerj def`'s penalty to test,
  example, fuzz and bench paths. Prose corpora, DSL queries and explicit sorts
  keep the engine's order (PR [#2125](https://github.com/xerj-org/xerj/pull/2125)).
- Autoindex: a heartbeat while an extract slot is full (PR
  [#1657](https://github.com/xerj-org/xerj/pull/1657)). An open section overlaps
  on a word (PR [#1700](https://github.com/xerj-org/xerj/pull/1700)). The help
  documents the bulk size maximum (PR [#1746](https://github.com/xerj-org/xerj/pull/1746),
  leninworld). Duplicate content is ranked by native path identity, and an
  unprojectable plan is refused before the snapshot is sealed (PRs
  [#1780](https://github.com/xerj-org/xerj/pull/1780),
  [#1781](https://github.com/xerj-org/xerj/pull/1781), amartya1523). The summary
  leads with an `ok:` line that spells out exit 3 (PR
  [#1913](https://github.com/xerj-org/xerj/pull/1913)). A rewritten `--prefix`
  warns and names the wildcard collision (PR
  [#1914](https://github.com/xerj-org/xerj/pull/1914)), and a literal `--` in a
  prefix is kept so `owner--repo` corpora verify (PR
  [#1906](https://github.com/xerj-org/xerj/pull/1906)). The help says a
  `.xerjignore` `!pattern` cannot re-include a hidden directory (PR
  [#1916](https://github.com/xerj-org/xerj/pull/1916)). A file that changes
  mid-run is skipped and recorded instead of aborting (PR
  [#1917](https://github.com/xerj-org/xerj/pull/1917)). Schema-only SQL is
  indexed as text (PR [#1987](https://github.com/xerj-org/xerj/pull/1987)), and
  a README whose first line is badges is not sniffed as JSON (PR
  [#1988](https://github.com/xerj-org/xerj/pull/1988)). Semantic verify errors
  include counts and datasets (PR [#2016](https://github.com/xerj-org/xerj/pull/2016),
  Jarvis636431). The field-report footer honours `--disable-feedback` and
  `XERJ_DISABLE_FEEDBACK` (PR [#1907](https://github.com/xerj-org/xerj/pull/1907)).
  The flood-stage fail-fast is PR [#2087](https://github.com/xerj-org/xerj/pull/2087),
  the stale-resume fix is PR [#2086](https://github.com/xerj-org/xerj/pull/2086),
  `--walk-only` is PR [#2006](https://github.com/xerj-org/xerj/pull/2006), and
  the budget is PR [#2102](https://github.com/xerj-org/xerj/pull/2102).
- Corpus Hub and corpora: the record corpora are published as Hub v2 hubpacks
  (PR [#1736](https://github.com/xerj-org/xerj/pull/1736)). Agents rate hub
  corpora with `xerj_corpus_feedback`, `xerj hub feedback` and automatic
  reviews (PR [#1886](https://github.com/xerj-org/xerj/pull/1886)). `xerj ask`
  exits 6 when the hub catalogue cannot be loaded (PR
  [#1905](https://github.com/xerj-org/xerj/pull/1905)). A named language routes
  to its corpora (PR [#2104](https://github.com/xerj-org/xerj/pull/2104)). The
  hub test harness runs cross-platform, with Windows file locking (PR
  [#1764](https://github.com/xerj-org/xerj/pull/1764), roedyrustam). `corpus
  add` and `xerj code` use a completed licence review when the detector reads
  UNKNOWN (PR [#1910](https://github.com/xerj-org/xerj/pull/1910)), and a
  BSD-3-Clause checkout no longer warns as BSD (PR
  [#1674](https://github.com/xerj-org/xerj/pull/1674)). A corpus name that its
  index prefix would rewrite is refused (PR
  [#1919](https://github.com/xerj-org/xerj/pull/1919)). A reference corpus skips
  CSV, HDF and simulation data (PR [#2014](https://github.com/xerj-org/xerj/pull/2014),
  Tong-bit-art).
- MCP, CLI and installer: `xerj_field_report` fills version and os when the
  agent omits them (PR [#2005](https://github.com/xerj-org/xerj/pull/2005)), and
  a down `XERJ_URL` explains how to start a node (PR
  [#2085](https://github.com/xerj-org/xerj/pull/2085)). The field-report verdict
  cap is 1000 characters (PR [#1847](https://github.com/xerj-org/xerj/pull/1847)),
  and unfilled optional placeholders are dropped on `--submit` (PR
  [#1851](https://github.com/xerj-org/xerj/pull/1851)). The `xerj --help`
  feedback blurb names `--submit` (PR [#1849](https://github.com/xerj-org/xerj/pull/1849)).
  `xerj init` stops printing a lone backslash in the MCP registration (PR
  [#1848](https://github.com/xerj-org/xerj/pull/1848)). The installer resolves
  the latest release without api.github.com (PR
  [#1852](https://github.com/xerj-org/xerj/pull/1852)) and uses the absolute
  `xerj` path in its next steps when `xerj` is not on PATH (PR
  [#1868](https://github.com/xerj-org/xerj/pull/1868)). The console tells an
  expired magic link how to recover (PR [#1675](https://github.com/xerj-org/xerj/pull/1675)).
  A document-style write to `/_memory` is explained (PR
  [#1683](https://github.com/xerj-org/xerj/pull/1683)), and a share link claim
  accepts `share_id` (PR [#1684](https://github.com/xerj-org/xerj/pull/1684)).
- Platform: macOS indexing no longer stalls under a memory cap, and shutdown no
  longer hangs (PR [#1677](https://github.com/xerj-org/xerj/pull/1677), dhkts1).
  Native Windows probes for system RAM, process RSS and disk (PR
  [#1766](https://github.com/xerj-org/xerj/pull/1766), roedyrustam). A bind
  failure says how to move each listener, including Windows error 10013 (PR
  [#1779](https://github.com/xerj-org/xerj/pull/1779), amartya1523). Startup is
  refused when the neural model cannot load (PR
  [#1877](https://github.com/xerj-org/xerj/pull/1877)).

### Testing

- Pinned filter `match`/`term`/`match_phrase` analyzer parity (PR
  [#1748](https://github.com/xerj-org/xerj/pull/1748), gyanu2507). The
  `pdf_worker` tests that mutate the environment are serialized to fix a flake
  (PR [#1743](https://github.com/xerj-org/xerj/pull/1743)). The §12.4 whole-hub
  gate is green, correct and runnable (PR [#1858](https://github.com/xerj-org/xerj/pull/1858)),
  and the sandboxed hub-e2e gate runs on a CI runner (PR
  [#1876](https://github.com/xerj-org/xerj/pull/1876)). Formatting after #1804
  (PR [#1960](https://github.com/xerj-org/xerj/pull/1960)).

### Documentation & site

- Corpus Hub v2 docs for `ask`, `hub` and the MCP hub tools (PR
  [#1728](https://github.com/xerj-org/xerj/pull/1728)), and the llms.txt agent
  surface (PR [#2048](https://github.com/xerj-org/xerj/pull/2048)). The README
  keeps the data directory outside the indexed tree (PR
  [#1686](https://github.com/xerj-org/xerj/pull/1686)) and opens with a
  one-line "what it is" (PR [#1923](https://github.com/xerj-org/xerj/pull/1923)).
  A table of 26 supported agent harnesses (PR
  [#2109](https://github.com/xerj-org/xerj/pull/2109), AK-Lmn). MCP tool counts
  (PRs [#1850](https://github.com/xerj-org/xerj/pull/1850),
  [#2118](https://github.com/xerj-org/xerj/pull/2118)). An MCP Registry
  `server.json` and an MCPB bundle builder (PR
  [#1922](https://github.com/xerj-org/xerj/pull/1922)).
- Site: a light theme by default (PR [#1920](https://github.com/xerj-org/xerj/pull/1920)),
  answer-first answer and compare pages (PRs
  [#1963](https://github.com/xerj-org/xerj/pull/1963),
  [#1966](https://github.com/xerj-org/xerj/pull/1966)), new answer and compare
  pages (PRs [#1964](https://github.com/xerj-org/xerj/pull/1964),
  [#1968](https://github.com/xerj-org/xerj/pull/1968)), the Corpus Hub v2 post
  and one cover series for the blog (PRs
  [#1915](https://github.com/xerj-org/xerj/pull/1915),
  [#1918](https://github.com/xerj-org/xerj/pull/1918)), sitemap content dates
  and noindex on markdown twins (PR [#1992](https://github.com/xerj-org/xerj/pull/1992)),
  a shorter home copy prompt (PR [#1953](https://github.com/xerj-org/xerj/pull/1953)),
  and a GA4 analytics tag on xerj.org (PR
  [#1962](https://github.com/xerj-org/xerj/pull/1962); the site only, nothing
  in the binary).

### Community & field reports

- Field report: the ES-YAML conformance run on a debug node (PR
  [#2127](https://github.com/xerj-org/xerj/pull/2127)).
- Field reports: the HTTP, MCP and console dogfood (PR
  [#1688](https://github.com/xerj-org/xerj/pull/1688)); autoindex and search on a
  Windows Electron/Vue project (PR [#1809](https://github.com/xerj-org/xerj/pull/1809),
  miku1130); reference-coding natural-language false positives (PR
  [#1936](https://github.com/xerj-org/xerj/pull/1936), tree-lancer); Windows MCP
  setup over an OSS checkout (PR [#1943](https://github.com/xerj-org/xerj/pull/1943),
  Zacharon); semantic verification diagnostics on macOS (PR
  [#2017](https://github.com/xerj-org/xerj/pull/2017), Jarvis636431); the
  autoindex store-size calibration (PR [#2103](https://github.com/xerj-org/xerj/pull/2103)).
- CLA signatures: HiChat-fog (#1600), eltociear (#1608), omertahaoztop (#1619),
  tizerluo (#1753), olituks (#1757), jacksonjp0311-gif (#1758), tripleaceme
  (#1765), amartya1523 (#1767), MokuLakshithReddy (#1806), miku1130 (#1810),
  roedyrustam (#1813), MrUranium1 (#1817), tree-lancer (#1938), Zacharon
  (#1942), ronitgupta138 (#1995), Jitin2102 (#2007), ravi-arnan (#2021),
  rudycelekli (#2053), AK-Lmn (#2108).

### Housekeeping

- Dependencies: `zip` 9 for RUSTSEC-2026-0367/0368/0369 (PR
  [#1869](https://github.com/xerj-org/xerj/pull/1869)) and `rustls-webpki` for
  RUSTSEC-2026-0382 (PR [#2008](https://github.com/xerj-org/xerj/pull/2008)).

## [1.0.0-rc.97] - 2026-10-19

Window since v1.0.0-rc.96: the first release that ships Corpus Hub v2 switched
on, plus a fix sweep driven by field reports. The fixes cover `xerj search` /
`xerj def` / `xerj code` on corpora that are not code-shaped, autoindex
dataset sprawl and finalize robustness, searches bounded by their deadline
while a merge publishes, ES cluster-settings semantics, the #1122
thread-spawn class in three more pools, and a stricter ES-YAML runner.
Contributors in this window: Tong-bit-art, jayhemnani9910, leninworld,
agammann, YongxinFu, bryanbans, Jarvis636431, eltociear, todie (carried in
#1503), jmbentolila, RumbleOnex, shyamsivakumar, seanpowless-dotcom,
HiChat-fog, omertahaoztop, camen-collab and dhkts1, with eval lessons from
chunxiaoxx (#1138).

### Corpus Hub v2

> **TODO(§12 gate), do not tag with this block as written.** Under
> `docs/CORPUS_HUB_V2_IMPLEMENTATION.md` §12, these notes may mention Hub v2
> only after §12.1–12.4 pass. Two numbers below can come only from those
> runs: the pack count (§12.1, `benchmarks/hub-backfill/REPORT.md` on
> `corpus-hub`) and the whole-hub pass rate (§12.4). Do not estimate them. If
> the gate does not pass, replace this section with the rc.96-style "not a
> feature of this release" paragraph and set `HUB_PUBKEY_HEX` back to empty
> for the cut.

- **Ask a pinned reference corpus, answered from a pre-indexed pack.**
  `xerj "<question>"` (the same as `xerj ask "<question>"`) picks one
  catalogued corpus for the question and answers from a signed, pre-indexed
  pack on this machine. The client routes the question itself, using the
  project's lockfiles and the per-corpus term sketches in the signed
  `catalogue.json`, so no hub server is involved. A symbol question (`--def`,
  a bare identifier, or "what is the value of X") returns the definition;
  other questions return passages with `file:line`. Every answer names the
  corpus, its pin and its licence, and approach-only licences carry a
  read-do-not-copy warning on every hit. An ambiguous question lists the
  candidates and exits 4, and `--corpus <slug>` picks one. Nothing is cloned
  or indexed on the user's machine: a pack is a snapshot of an index that was
  already built. Answers need a local XERJ node. If none answers on the
  configured URL, `xerj ask` starts one on a free port from 9300 up, and
  `xerj hub stop` stops only that node.
- **What the catalogue holds.** hub.xerj.org lists **TODO(§12.1): N** packs.
  From a clean machine, against the published assets, **TODO(§12.4): N of N**
  catalogued corpora passed `xerj hub pull` plus one `--def` lookup plus one
  question. The gate is 100%; a corpus that fails is delisted, not shipped.
- **Signed end to end.** The binary embeds the hub's ed25519 public key (PR
  [#1505](https://github.com/xerj-org/xerj/pull/1505)). The catalogue and
  every pack must pass the signature and sha256 checks before anything is
  extracted, and a tampered pack or catalogue is refused. A catalogue URL
  that serves an HTML page with status 200, which is what a static host does
  for an unknown path, reads as "catalogue unavailable" and never as data
  (PR [#1587](https://github.com/xerj-org/xerj/pull/1587)).
- **Pulls are bounded.** `xerj hub pull <slug>` fetches a pack; `xerj hub
  sync`, `status`, `stop` and `config` manage them. `xerj ask` pulls on its
  own only packs of 50 MB or less, and never a pack whose licence class is
  Excluded. For a larger pack it prints the `xerj hub pull` line with the
  size. The packs directory has a 4 GB budget, and the least recently used
  packs are evicted first. `--offline` never touches the network.
- **No hosted node in this release.** The hosted search and answer routes
  are out of scope for this release (§12). Answers come from local packs
  only, and `--hosted on` says that this build has no hosted client.
- **MCP tools never download.** `xerj_hub_find`, `xerj_hub_def` and
  `xerj_hub_ask` run the same answer flow as the CLI, with two differences: a
  tool call never pulls a pack and never fetches the catalogue. A corpus that
  is not on the machine is answered with the `xerj hub pull <slug>` line to
  run, and with no cached catalogue the tool says to run `xerj hub sync`.
  The tools, and the hub files `xerj init` writes, appear only in a build
  that has a hub key (PR
  [#1629](https://github.com/xerj-org/xerj/pull/1629)).
- **What ranks the answer.** Passage answers come from the same BM25
  pipeline as `xerj code`. The default embedder is lexical feature-hashing,
  so nothing on the hub path is neural unless the node runs with
  `--embed-mode neural`. No performance or token-saving figure is claimed
  here. The step-10 measurement (#1385) governs any such claim and has not
  been published.
- **How packs are built and gated** (CI and operator tooling):
  `xerj hub build-catalogue`, the signed catalogue builder (PR
  [#1513](https://github.com/xerj-org/xerj/pull/1513)). In `hub-packs.yml`,
  each pack is pulled into an empty `HOME` and must answer one `--def` and
  one question before its release is published (§12.3, PR
  [#1514](https://github.com/xerj-org/xerj/pull/1514)). Every run records a
  backfill-report row for every corpus, failures included (§12.1, PR
  [#1522](https://github.com/xerj-org/xerj/pull/1522)). The delisting guard
  refuses a "previous catalogue" that is not one (PRs
  [#1582](https://github.com/xerj-org/xerj/pull/1582),
  [#1588](https://github.com/xerj-org/xerj/pull/1588)). The release gate
  itself is written down in §12 (PR
  [#1485](https://github.com/xerj-org/xerj/pull/1485)).

### Fixed

- **`match` / `term` / `terms` on an `index: false` + `doc_values: true`
  `text` field answer the same before and after a flush** (issue
  [#1509](https://github.com/xerj-org/xerj/issues/1509), PR
  [#1517](https://github.com/xerj-org/xerj/pull/1517), leninworld). Before the
  fix, the memtable answered on tokens or not at all, while the flushed
  segment answered on the whole value. Both now follow the ES whole-value,
  case-sensitive doc-values semantics. Coverage spans 15 direct and wrapped
  query shapes, `size: 0` totals included.
- **A search blocked behind a merge publication stops at its deadline**
  (PR [#1638](https://github.com/xerj-org/xerj/pull/1638)). It now returns
  HTTP 200 with `timed_out: true` and `hits.total.relation: gte`. Before, it
  returned a 500 that retrying clients turned into 76–300 s waits on a node
  merging a 1.2 M-document dataset. This bounds the wait; it does not make
  the merge faster.
- **`PUT _cluster/settings` merges into the stored settings, as ES does**
  (PR [#1595](https://github.com/xerj-org/xerj/pull/1595), jayhemnani9910).
  One update used to replace the whole `persistent` / `transient` object, and
  the nested form was stored nested and never applied. Settings are now keyed
  by their flat name, and `null` removes one.
  **`cluster.routing.allocation.disk.threshold_enabled: false` is honoured**
  (PR [#1596](https://github.com/xerj-org/xerj/pull/1596),
  jayhemnani9910): it lifts the flood-stage `read_only_allow_delete` block,
  as ES does.
- **ES-compat responses** — `GET /{index}` and `_settings` report the
  persisted `creation_date` instead of the current time on every request
  (PR [#1589](https://github.com/xerj-org/xerj/pull/1589), Tong-bit-art; issue
  [#1573](https://github.com/xerj-org/xerj/issues/1573) is still open on the
  tracker at the cut). `GET /{concrete-missing}/_mapping` returns 404
  `index_not_found_exception` instead of `200 {}`, and a wildcard miss is
  unchanged (PR [#1611](https://github.com/xerj-org/xerj/pull/1611)). A bare
  `_count` no longer counts autoindex file-card anchors, while explicit
  queries still see them (PR
  [#1624](https://github.com/xerj-org/xerj/pull/1624)). A bad `hybrid`
  clause error names the `{query, weight}` shape (PR
  [#1628](https://github.com/xerj-org/xerj/pull/1628)). The slow-query log
  prints the effective body (`match_all`, `knn`, or both) instead of an empty
  `query=` (PR [#1630](https://github.com/xerj-org/xerj/pull/1630)).
- **`xerj search` and `xerj def` work on corpora that are not code-shaped**
  (issue [#1601](https://github.com/xerj-org/xerj/issues/1601), PR
  [#1602](https://github.com/xerj-org/xerj/pull/1602)). On a markdown or log
  corpus, every query was an HTTP 400 because the client named
  `defs` / `name` / `code` fields that autoindex's strict mappings did not
  have. The client now reads the mapping and drops the clauses for fields
  that are not mapped. A field that any index maps as numeric is dropped
  too, in `xerj code` (issue
  [#1284](https://github.com/xerj-org/xerj/issues/1284), PR
  [#1605](https://github.com/xerj-org/xerj/pull/1605)) and in `search` /
  `def` (PR [#1617](https://github.com/xerj-org/xerj/pull/1617)), so one
  `long` `title` no longer fails the whole wildcard query. A prefix that
  matches nothing exits 1 and lists the prefixes the node has (PR
  [#1618](https://github.com/xerj-org/xerj/pull/1618)).
- **`xerj def` ranks the declaration first** (PR
  [#1614](https://github.com/xerj-org/xerj/pull/1614)). Structs, classes,
  functions and other preferred kinds outside tests, examples, fuzz and
  benches, and `pub` / `export` items, now outrank type aliases and `mod`
  lines. On sled, `pub struct Db` moved from 7th to 1st.
- **`xerj code`** — `--mode semantic` queries the field each index actually
  embeds instead of only `body` (PR
  [#1621](https://github.com/xerj-org/xerj/pull/1621)), and `xerj corpus
  index` no longer indexes the root `corpus.json` manifest (PR
  [#1622](https://github.com/xerj-org/xerj/pull/1622)). A page cut short by
  the node's search deadline is marked partial instead of being printed as
  complete (PR [#1637](https://github.com/xerj-org/xerj/pull/1637)). The
  vector leg says that the default embedder is lexical feature-hashing, and
  translation catalogs and minified bundles sort behind implementing files
  (PR [#1603](https://github.com/xerj-org/xerj/pull/1603), partial for field
  report [#1504](https://github.com/xerj-org/xerj/issues/1504), which stays
  open).
- **autoindex no longer turns a config-heavy repo into one index per file**
  (PR [#1610](https://github.com/xerj-org/xerj/pull/1610)). Small JSON, YAML
  and XML clusters fold into their scope's document dataset, and each scope
  keeps at most 12 small structured datasets. `autoindex map` and `xerj_map`
  page their field detail. On the engine tree this went from 161 datasets
  and a 376 KB map to 9 datasets and a 34 KB map.
- **autoindex finalize and ingest robustness.** A count that times out or
  errors is no longer read as 0 and no longer aborts a run that wrote its
  records (PR [#1639](https://github.com/xerj-org/xerj/pull/1639)). A nested
  JSON value stored as one field is capped at 64 KiB, after a 12 MB value
  wedged ingest (PR [#1640](https://github.com/xerj-org/xerj/pull/1640)).
  The zero-live refusal names the journal directory, which a new
  `--data-dir` reuses (PR
  [#1616](https://github.com/xerj-org/xerj/pull/1616)). Prose sections are
  cut on word boundaries and carry `start_line` (PR
  [#1623](https://github.com/xerj-org/xerj/pull/1623)). `autoindex status`
  marks another endpoint's journal as `other endpoint <url>` (PR
  [#1630](https://github.com/xerj-org/xerj/pull/1630)).
- **autoindex output agents read correctly.** A second folder says "one
  folder per run" and shows the two-run `--prefix` form (issue
  [#1586](https://github.com/xerj-org/xerj/issues/1586), PR
  [#1590](https://github.com/xerj-org/xerj/pull/1590), Tong-bit-art). A usage
  error prints the error and one `--help` pointer instead of 390 lines of
  help on stdout (PR [#1592](https://github.com/xerj-org/xerj/pull/1592),
  Tong-bit-art). Exit 3 is followed by a line saying that it means success
  (PR [#1643](https://github.com/xerj-org/xerj/pull/1643)).
- **`xerj init` teaches `--prefix` on every surface it writes**: the
  AGENTS.md section (PR [#1584](https://github.com/xerj-org/xerj/pull/1584)),
  the project skill (PR [#1585](https://github.com/xerj-org/xerj/pull/1585),
  `INIT_FILES_VERSION` 2), and the hub skill, `--help` and closing text (PR
  [#1606](https://github.com/xerj-org/xerj/pull/1606), version 3). A
  pristine older copy is upgraded with a `.bak`, and an edited one is kept
  (issue [#1580](https://github.com/xerj-org/xerj/issues/1580) still open on
  the tracker at the cut).
- **CLI.** Local admin-key discovery finds `~/xerj-data` and `./.xerj-data`,
  the node records a per-port pointer to its key, and a 401/403 says
  "authentication failed" instead of "no node reachable" (PR
  [#1625](https://github.com/xerj-org/xerj/pull/1625)). A flag written
  before the subcommand (`xerj --url URL corpus index …`) is accepted (PR
  [#1642](https://github.com/xerj-org/xerj/pull/1642)). Wrapped help lines
  keep their indent (PR [#1626](https://github.com/xerj-org/xerj/pull/1626)).
  `xerj feedback` scopes its catalog sentence to the latest run and keeps
  `--agent` in the rerun line (PR
  [#1627](https://github.com/xerj-org/xerj/pull/1627)). The installers close
  with `~/xerj-data` instead of `./data`, `xerj mcp --help` counts the tools
  it serves, and an unknown query type links a page that exists (PR
  [#1629](https://github.com/xerj-org/xerj/pull/1629)).
- **`xerj_map` filters index globs before its 100-dataset cap** (PR
  [#1591](https://github.com/xerj-org/xerj/pull/1591), agammann). Before, 100
  larger unrelated datasets could hide a matching one and report a false
  `matched: 0`. This entry was under `[Unreleased]`.
- **A failed thread spawn degrades instead of aborting the node, in three
  more places** (partial for issue
  [#1122](https://github.com/xerj-org/xerj/issues/1122), which stays open).
  The governor threads and drain-free spawn use the new `try_spawn` helper
  and spawn gate (PR [#1460](https://github.com/xerj-org/xerj/pull/1460)).
  The autoindex object-fetch pool falls back to sequential fetches (PR
  [#1520](https://github.com/xerj-org/xerj/pull/1520)). The ingest,
  background and merge rayon pools retry at one thread and then run inline
  (PR [#1604](https://github.com/xerj-org/xerj/pull/1604)).
- **Decision write-back keeps growing on split votes** (issue
  [#1094](https://github.com/xerj-org/xerj/issues/1094), closed during the
  window; PR [#1503](https://github.com/xerj-org/xerj/pull/1503), carries
  todie's #1467). With the local decide tier armed, a history vote whose
  winning share is below the new `[decisions] escalate_below` setting
  (default 0.8) goes to the local head. The answer is written back with one
  cached document per decided text, so repetition cannot out-vote the
  history. With no local tier armed, nothing changes. The Banking77 replay
  gate has not been re-measured. The setting count goes from 132 to 133.

### Added

- **autoindex reads journalctl / ISO-timestamp syslog and vsftpd `xferlog`
  as per-line log records with parsed fields** (PR
  [#1508](https://github.com/xerj-org/xerj/pull/1508), Tong-bit-art, from
  field report [#1504](https://github.com/xerj-org/xerj/issues/1504), which
  stays open for its other suggestions). Before, both were shredded into
  prose chunks.

### Security

- **The server no longer prints the admin API key on first boot** (PR
  [#1633](https://github.com/xerj-org/xerj/pull/1633)). The key is written
  to `<data_dir>/admin.key` (mode 0600) first, and the notice names the file.
  An agent that captured server stdout used to end up holding the
  credential.

### Testing

- **The ES-YAML runner enforces `catch:`** (PR
  [#1583](https://github.com/xerj-org/xerj/pull/1583)). It used to skip every
  expected-error step, so "0 failed" said nothing about requests that ES
  rejects. On origin/main the strict run had 151 failures (91 aggregations,
  37 search, 12 indices, 7 scroll, 2 bulk, 2 vectors). They are listed in
  `catch-lenient.txt` and keep their old behaviour, and every other case is
  now strict. Four new cases record known divergences and wait for fixes:
  `null_value` (PR [#1553](https://github.com/xerj-org/xerj/pull/1553)),
  `enabled: false` (PR [#1574](https://github.com/xerj-org/xerj/pull/1574)),
  `_source` includes/excludes (PR
  [#1575](https://github.com/xerj-org/xerj/pull/1575)), and dynamic-template
  mapping types (PR [#1576](https://github.com/xerj-org/xerj/pull/1576)).

### Documentation & site

- The agentic-memory recipe, in the repo and on xerj.org, gains "Before you
  trust a recall number", chunxiaoxx's eval lessons from #1138 with credit
  (PRs [#1527](https://github.com/xerj-org/xerj/pull/1527),
  [#1535](https://github.com/xerj-org/xerj/pull/1535); issue #1138 stays
  open). A Japanese README (PR
  [#1607](https://github.com/xerj-org/xerj/pull/1607), eltociear). The
  contributing guide describes the conformance gate as it runs (PR
  [#1498](https://github.com/xerj-org/xerj/pull/1498), Jarvis636431). The
  dead `xerj admin magic-link` is gone from the demo and design docs (PR
  [#1615](https://github.com/xerj-org/xerj/pull/1615)). Sitemap refresh (PR
  [#1632](https://github.com/xerj-org/xerj/pull/1632)).
- Accuracy polish, one claim per PR. Site and docs pages: #1530, #1532,
  #1536, #1537, #1539, #1545, #1546, #1547, #1549, #1551, #1555, #1557,
  #1559, #1562, #1563, #1571. Markdown: #1525, #1528, #1529, #1566, #1567,
  #1568, #1578. `SECURITY_MODEL.md` line references: #1538, #1540, #1541,
  #1542, #1543, #1579, #1581. Rustdoc: #1544, #1552, #1556, #1560, #1564,
  #1569, #1570, #1572. Scripts: #1533, #1534, #1548, #1550, #1554, #1558,
  #1561, #1577.

### Community & field reports

- Field reports: a reference-coding probe on unfamiliar Rust (PR
  [#1511](https://github.com/xerj-org/xerj/pull/1511), jmbentolila);
  reference coding and autoindex on Windows (PR
  [#1518](https://github.com/xerj-org/xerj/pull/1518), RumbleOnex); Qabot
  reference-coding retrieval on rc.95 (PR
  [#1519](https://github.com/xerj-org/xerj/pull/1519), shyamsivakumar);
  search vs `git grep` on cudarc and candle (PR
  [#1598](https://github.com/xerj-org/xerj/pull/1598), seanpowless-dotcom),
  which led to #1633; a ZCode (GLM) workstation setup (PR
  [#1599](https://github.com/xerj-org/xerj/pull/1599), HiChat-fog); local
  repos plus hub corpora (PR
  [#1613](https://github.com/xerj-org/xerj/pull/1613), omertahaoztop); the
  engine and sled dogfood (PR
  [#1620](https://github.com/xerj-org/xerj/pull/1620)); and autoindex at
  defaults stalling a 13.8k-file repo on rc.96 (PR
  [#1636](https://github.com/xerj-org/xerj/pull/1636), camen-collab). That
  stall is issue [#1635](https://github.com/xerj-org/xerj/issues/1635), which
  stays open and is not fixed in this release.
- CLA signatures: jmbentolila (#1512), jayhemnani9910 (#1594), agammann
  (#1597), cursoragent (#1609), dhkts1 (#1646). A `funding.json` manifest
  for FLOSS/fund (PR [#1676](https://github.com/xerj-org/xerj/pull/1676)).

### Housekeeping

- CI: the workflow-timeout guard rejects job declarations it cannot parse
  (PR [#1472](https://github.com/xerj-org/xerj/pull/1472), YongxinFu). SEO
  dates do not trust shallow-clone history (PR
  [#1476](https://github.com/xerj-org/xerj/pull/1476), bryanbans).
  Docs-only PRs skip the heavy Rust jobs and fail open (PR
  [#1477](https://github.com/xerj-org/xerj/pull/1477)). The 8-target release
  matrix runs nightly and on tags, not on every push to main (PR
  [#1565](https://github.com/xerj-org/xerj/pull/1565)).
- The metrics worker source is synced with the deployed worker, and its
  `/trending-history` route is documented (PRs #1524, #1531).

## [1.0.0-rc.96] - 2026-10-15

Window since v1.0.0-rc.95: a fix-and-honesty sweep across search tie order,
dynamic-mapping doc values, autoindex progress and resume reporting, the
finalize gate, Windows home-directory resolution and `xerj init`, plus two
dependency security updates. Contributors in this window: Tong-bit-art,
todie, fjjjuv, redwolf2019, riazrahaman, sebollin, Tyagiquamar, driphtyio and
mininxd, whelanh and Jarvis636431. Parts of
this section's wording for the PRs merged before 2026-10-14 are adapted from
todie's draft release PR #1397, with thanks.

**Corpus Hub v2 is not a feature of this release.** Its code is in the tree
(PR [#1423](https://github.com/xerj-org/xerj/pull/1423)), but this build has
no hub signing key compiled in, so no pre-indexed pack can be verified and
none is downloaded. PR [#1491](https://github.com/xerj-org/xerj/pull/1491)
makes that explicit on every hub surface: `xerj ask`, `xerj "<question>"`,
`xerj hub sync` and `xerj hub pull` exit 5 with one plain message instead of a
catalogue parse error, the MCP hub tools return the same message, `xerj init`
no longer writes the hub skill or advertises `xerj "<question>"`, and
`xerj --help` says the command is unavailable in this build. The
`xerj code` / `xerj corpus` reference-coding path is unchanged and works as
before.

### Fixed

- **Inferred `text` fields take the ES dynamic-mapping `doc_values: false`
  default** (PR [#1329](https://github.com/xerj-org/xerj/pull/1329),
  Tong-bit-art) — an inferred `text` field kept `doc_values: true` while a
  declared one defaulted to `false`, so at flush the segment wrote a
  whole-value keyword column and `terms` answered against raw untokenized
  strings (another instance of the "answers change at flush" class). Inferred
  `text` now matches declared `text` and ES; `.keyword` sub-fields keep their
  column. Test follow-ups: PR
  [#1331](https://github.com/xerj-org/xerj/pull/1331) (`agg_bucket_cap_terms`)
  and PR [#1356](https://github.com/xerj-org/xerj/pull/1356) (Tong-bit-art,
  the `graph_expand` fixture declares its `edges` mapping).
- **`term`/`terms` on a `text` field with `doc_values: true` no longer drop
  to 0 hits after a flush** (PR
  [#1489](https://github.com/xerj-org/xerj/pull/1489), Tong-bit-art) — the
  doc-values term prefilter answered against the whole-value column for
  analyzed text; it now declines analyzed text and the query takes the full,
  correct path.
- **Total tie order (#191) held on two more `term` routes** (issue
  [#1359](https://github.com/xerj-org/xerj/issues/1359), PR
  [#1393](https://github.com/xerj-org/xerj/pull/1393), todie) — a `term` on a
  `text` field without doc values (the post-#1329 default) and a standalone
  `term` on a `keyword` field across an unflushed memtable plus flushed
  segments could return pages that were not prefixes of each other. Bounded
  pages now keep `score DESC, seq_no ASC, _id ASC` across segments.
- **`xerj autoindex` accepts a whole-file size cap below 1 GB** (issue
  [#1233](https://github.com/xerj-org/xerj/issues/1233), PR
  [#1321](https://github.com/xerj-org/xerj/pull/1321)) — new
  `--max-file-mb` / `--max-file-bytes` beside `--max-file-gb` (default still
  2 GB); two size flags together are a usage error. Existing indexes resume
  with the same prepared-records identity.
- **An aborted autoindex run no longer prints the "searchable" success
  footer** (PR [#1318](https://github.com/xerj-org/xerj/pull/1318), partial
  for issue [#1183](https://github.com/xerj-org/xerj/issues/1183), since
  closed) — the salvage path now says the records are present but coverage is
  not guaranteed, and points at `xerj corpus index <name> --fresh`.
- **Finalize progress and old-server detection for corpus resumes** (issue
  [#1267](https://github.com/xerj-org/xerj/issues/1267), still open) — the
  catalog read-back walk is its own `finalize-catalog-readback` progress
  phase with a real denominator instead of a frozen N/N bar (PR
  [#1320](https://github.com/xerj-org/xerj/pull/1320)), and the client times
  one verify window before the walk and warns, without aborting, when the
  server looks older than the client's fast-path generation (PR
  [#1430](https://github.com/xerj-org/xerj/pull/1430)). The server-side
  walk cost is not fixed by either; the issue stays open.
- **`_refresh` / flush skip empty shards before the shared finalize gate**
  (issue [#1226](https://github.com/xerj-org/xerj/issues/1226)) — PR
  [#1319](https://github.com/xerj-org/xerj/pull/1319) skips empty shards when
  building the flush/refresh fan-out; PR
  [#1456](https://github.com/xerj-org/xerj/pull/1456) (carries todie's
  #1399) also checks inside `do_flush_shard` before taking the process-wide
  gate, and resolves concrete index names and aliases in O(1) instead of
  listing (and computing stats for) every open index. No latency figure is
  claimed here; each change is pinned by a regression test.
- **Multi-index ingest against the memory breaker** (issue
  [#1250](https://github.com/xerj-org/xerj/issues/1250), closed during the
  window) — the parent breaker watermark is live-tunable through the ES
  dynamic cluster setting `indices.breaker.total.limit` (a percentage of
  machine memory or an absolute size), without a restart, and the 429 message
  names the knob (PR [#1323](https://github.com/xerj-org/xerj/pull/1323)).
  The autoindex bulk client damps its AIMD oscillation (a 15 s episode
  cooldown, an ssthresh, a permit per HTTP attempt) and the breaker's drain
  churn is reduced (PR [#1482](https://github.com/xerj-org/xerj/pull/1482),
  carries todie's #1400). Per-index admission and a server-side shed signal
  are not part of this.
- **Native `/v1/cluster/health` reports the configured topology** (PR
  [#1316](https://github.com/xerj-org/xerj/pull/1316), partial for issue
  [#1170](https://github.com/xerj-org/xerj/issues/1170)) — it hardcoded one
  node; it now agrees with `_cluster/health` and flags a degraded transport.
  This is the configured floor, not live ring state: the Raft ring is still
  not wired to the engine and #1170 stays open.
- **Corpus builder refuses two silent wrong-result paths** (PR
  [#1315](https://github.com/xerj-org/xerj/pull/1315), partial for issue
  [#1110](https://github.com/xerj-org/xerj/issues/1110), which stays open) —
  a missing source root used to prune every stored envelope with exit 0, and
  records with an empty id shipped together and collapsed to one `_id`. Both
  now fail loudly. PR [#1500](https://github.com/xerj-org/xerj/pull/1500) (carries
  todie's #1468) also fails the build on an unreadable or invalid stored
  envelope and deletes stale shards before writing new ones, and adds a
  `cisa-kev` pack definition (not published or signed). The fresh-machine
  portability matrix of #1110 is still open.
- **Native Windows: home directory falls back to `%USERPROFILE%`** when
  `HOME` is unset (PR [#1427](https://github.com/xerj-org/xerj/pull/1427)) —
  `xerj init` skipped every home-scoped agent config and caches resolved
  relative to the working directory. The shared helper is now used by the
  MCP server, autoindex, the server and local auth too (PRs
  [#1436](https://github.com/xerj-org/xerj/pull/1436),
  [#1437](https://github.com/xerj-org/xerj/pull/1437),
  [#1438](https://github.com/xerj-org/xerj/pull/1438),
  [#1439](https://github.com/xerj-org/xerj/pull/1439)).
- **`xerj init` never edits `.mcp.json` or `AGENTS.md` without a backup**
  (PR [#1411](https://github.com/xerj-org/xerj/pull/1411)) — a failed backup
  copy was ignored and an unreadable `AGENTS.md` was replaced; both now leave
  the file untouched. **Teaching files are version-marked** (PR
  [#1444](https://github.com/xerj-org/xerj/pull/1444)): a re-run upgrades an
  unedited older copy (with a `.bak`), keeps an edited one and says so, and
  treats an unmarked file as the user's.
- **`xerj code` renders a symbol hit's code, not its provenance digest**
  (PR [#1431](https://github.com/xerj-org/xerj/pull/1431), carries todie's
  #1390) — `*-symbols` hits printed the 42-char `ax_file` digest as the
  passage; `ax_*` keys are now excluded from the fallback, and `code` is used
  only on symbol-shaped records so a short raw-JSON `code` field (an error
  code, a CWE id) does not regress.
- **`xerj autoindex map --prefix` is honoured** (PR
  [#1459](https://github.com/xerj-org/xerj/pull/1459), carries todie's
  #1391) — the flag was ignored and every corpus printed; scoping now happens
  in the server query as well as client-side, and a blank `wall_seconds` no
  longer renders as `wall s`.
- **Console bootstrap banner names a recovery that exists** (issue
  [#1424](https://github.com/xerj-org/xerj/issues/1424), PR
  [#1426](https://github.com/xerj-org/xerj/pull/1426), Tong-bit-art) — it
  suggested `xerj admin magic-link`, which the binary rejects; it now says to
  restart the node, which prints a fresh link before enrolment.
- **Console: a superseded render never paints over a newer route** (issue
  [#1425](https://github.com/xerj-org/xerj/issues/1425), PR
  [#1428](https://github.com/xerj-org/xerj/pull/1428)) — a slow view could
  snap back over the one the user had moved to; also the root cause of a
  flaky browser test.
- **`xerj search` on multi-word natural-language queries** (partial for
  issue [#1364](https://github.com/xerj-org/xerj/issues/1364), which stays
  open) — for queries of three or more distinct tokens, short glue words
  (EN/ES/PT/FR/IT/DE) are dropped from the match clauses so a symbol named
  `de` no longer wins, and passages on autoindex code documents prefer the
  `code`/`body` field over a bare name (PR
  [#1484](https://github.com/xerj-org/xerj/pull/1484), carries todie's
  #1396).
- **Whole-file `xerj search` / `xerj def` hits print a 1-based line** (issue
  [#1365](https://github.com/xerj-org/xerj/issues/1365), PR
  [#1502](https://github.com/xerj-org/xerj/pull/1502), carries todie's
  #1392) — the engine also recomputes a refined code passage's start line. A
  hit whose best passage is a `defs`/`title` field still prints the bare
  path, a residual left open.
- **A failed thread spawn degrades instead of aborting the process**
  (partial for issue [#1122](https://github.com/xerj-org/xerj/issues/1122),
  which stays open) — under memory exhaustion `std::thread::spawn` panics,
  and the engine is `panic=abort`. These paths now handle the error: the
  `xerj share` tunnel fails on its own (PR
  [#1479](https://github.com/xerj-org/xerj/pull/1479)), an extractor reader
  thread fails its file rather than the whole run (PR
  [#1480](https://github.com/xerj-org/xerj/pull/1480)), and autoindex index
  workers, sync replay and object fetch degrade (PR
  [#1481](https://github.com/xerj-org/xerj/pull/1481)), and the phase-A scan
  pool falls back to the calling thread (PR
  [#1501](https://github.com/xerj-org/xerj/pull/1501)). Spawn sites elsewhere
  are tracked by the issue.
- **`xerj` and `xerj index` refuse a missing or non-numeric flag value**
  (issue [#1435](https://github.com/xerj-org/xerj/issues/1435), PR
  [#1452](https://github.com/xerj-org/xerj/pull/1452)) — `xerj --data-dir`
  with no value booted on `./data` and swallowed the next flag; it is now a
  usage error.
- **`xerj --help` states the served MCP tool count** (issue
  [#1443](https://github.com/xerj-org/xerj/issues/1443), PR
  [#1455](https://github.com/xerj-org/xerj/pull/1455)) — computed from the
  served tool list instead of a stale constant.

### Added

- **`xerj code --fuse-symbols`, opt-in** (issue
  [#1237](https://github.com/xerj-org/xerj/issues/1237), PR
  [#1457](https://github.com/xerj-org/xerj/pull/1457), carries todie's
  #1401) — autoindex symbol records now carry `end_line` and a searchable
  `impl` body slice (capped at 16 KiB); with the flag, a second BM25 leg over
  symbol bodies is fused with the file ranking by Reciprocal Rank Fusion
  (`k = 60`). Without the flag the default ranking is unchanged.
  Pre-existing state dirs resume without a rebuild.
- **`--auto-port`, opt-in** (PR
  [#1487](https://github.com/xerj-org/xerj/pull/1487), mininxd) — with
  `--auto-port`, `XERJ_AUTO_PORT=true` or `[server] auto_port = true`, a
  node whose ports are taken binds the next free ones, and a data dir held by
  another live node moves to a `<data_dir>-N` sibling, so several agents can
  start nodes side by side. Off by default: a taken port stays a fatal
  startup error.

### Performance

- **autoindex indexes the plan once per snapshot** instead of rescanning it
  for every file (PR [#1483](https://github.com/xerj-org/xerj/pull/1483),
  carries part of todie's #1394). Also from #1394: read-lease counting moved
  off writer-hot cache lines and the graveyard lock (PR
  [#1496](https://github.com/xerj-org/xerj/pull/1496)), and the PDF worker
  supervisor wakes on pipe EOF instead of a 20 ms sleep poll (PR
  [#1486](https://github.com/xerj-org/xerj/pull/1486)). No figures are
  claimed here.

### Security

- **`quinn-proto` 0.11.14 → 0.11.19** (PR
  [#1357](https://github.com/xerj-org/xerj/pull/1357), Tong-bit-art) —
  `RUSTSEC-2026-0338` through `-0342`.
- **`calamine` 0.26.1 → 0.36.1 and `zip` 2 → 8** (issue
  [#1358](https://github.com/xerj-org/xerj/issues/1358), PR
  [#1398](https://github.com/xerj-org/xerj/pull/1398), todie) — removes
  `quick-xml` 0.31.0 (`RUSTSEC-2026-0097`, `-0098`) from the dependency
  graph. The `.xls` path stays process-isolated as in rc.95.

### Measured

- **The #1062 wrong-and-confident detection gate was run through the real
  `_watcher` path and it FAILS** (issue
  [#1108](https://github.com/xerj-org/xerj/issues/1108), PR
  [#1322](https://github.com/xerj-org/xerj/pull/1322)) — the unmeasured half
  of the rc.80 `_watcher` work. On the rc.94 release binary, a real
  `PUT /_watcher/watch` with `condition.xerj_decide` (one detection watch per
  label, one-vs-rest) evaluated a 120-email boundary tier and wrote 120 fire
  records into `.xerj_alert_fires`; the measurement reads only those fires.
  **Wrong-and-confident = 0.2449** (24 of 98 fires with confidence ≥ 0.8 were
  wrong) against the **≤ 0.05** bar. The confidence compared is the raw
  `/_decide` confidence the watcher fires on; `p_cal` was absent (the vote
  history has no labelled outcome pairs to fit), and because 18 of the 24
  confident errors sit at raw `p` = 1.000 a monotonic `p_cal` could not change
  the verdict anyway. Published under the loses-does-not-ship rule: the
  `_watcher` mechanism is real, but its default vote is not good enough to ship a
  detection watch as a "confidently correct" default, and no doc claims it is.
  Raw results, corpus generator and harness in
  `benchmarks/systemone-watcher-gate/`. Closes #1108.

### Documentation & site

- Docs that named commands which do not exist were corrected: the
  troubleshooting page's `xerj support-bundle` (issue
  [#1433](https://github.com/xerj-org/xerj/issues/1433), PR
  [#1434](https://github.com/xerj-org/xerj/pull/1434)), the upgrades page's
  `xerj migrate` / `xerj verify` (issue
  [#1440](https://github.com/xerj-org/xerj/issues/1440), PR
  [#1451](https://github.com/xerj-org/xerj/pull/1451)), the rerank egress
  table's `--interval` (issue
  [#1441](https://github.com/xerj-org/xerj/issues/1441), PR
  [#1442](https://github.com/xerj-org/xerj/pull/1442)), and the
  reference-coding opener prompt's removed `xc` scripts (PR
  [#1450](https://github.com/xerj-org/xerj/pull/1450)); the engine README's
  License link now resolves (issue
  [#1445](https://github.com/xerj-org/xerj/issues/1445), PR
  [#1446](https://github.com/xerj-org/xerj/pull/1446)).
- `llms.txt` reaches the existing `xerj corpus` / `xerj code` corpus path
  with commands that work (PR
  [#1371](https://github.com/xerj-org/xerj/pull/1371)); top-nav HUB, X and
  Discord links (PR [#1352](https://github.com/xerj-org/xerj/pull/1352));
  a Simplified Chinese README (PR
  [#1341](https://github.com/xerj-org/xerj/pull/1341), redwolf2019); the
  Trendshift badge (PR [#1458](https://github.com/xerj-org/xerj/pull/1458)).
- Planning documents, not shipped features: the Corpus Hub v2 implementation
  plan (PR [#1376](https://github.com/xerj-org/xerj/pull/1376)) and its
  pre-registered proof-of-value measurement (PR
  [#1385](https://github.com/xerj-org/xerj/pull/1385)); a blog note that the
  #1138 trajectory corpus gap has been seeded (PR
  [#1314](https://github.com/xerj-org/xerj/pull/1314), issue
  [#1138](https://github.com/xerj-org/xerj/issues/1138) stays open).

### Community & field reports

- Field reports: ES-compat flush-parity verification of rc.95 (PR
  [#1330](https://github.com/xerj-org/xerj/pull/1330), Tong-bit-art), Hermes
  agent semantic search over an agent kanban board (PR
  [#1354](https://github.com/xerj-org/xerj/pull/1354), riazrahaman),
  natural-language search on a Spanish-language R package (PR
  [#1367](https://github.com/xerj-org/xerj/pull/1367), sebollin),
  shared-cluster retrieval and an `llms.txt` audit (PR
  [#1373](https://github.com/xerj-org/xerj/pull/1373), todie), Windows
  autoindex + MCP (PR [#1420](https://github.com/xerj-org/xerj/pull/1420),
  fjjjuv), a Hermes agent autoindexing skills and blog corpora (PR
  [#1492](https://github.com/xerj-org/xerj/pull/1492), driphtyio), and two
  reference-coding reports, end to end and across the Omarchy-port and
  Fedora repos (PRs [#1463](https://github.com/xerj-org/xerj/pull/1463),
  [#1465](https://github.com/xerj-org/xerj/pull/1465), whelanh).
- CLA signatures: Tyagiquamar (#1327), Tong-bit-art (#1328), redwolf2019
  (#1342), riazrahaman (#1355), sebollin (#1366), fjjjuv (#1421), todie
  (#1429), mininxd (#1488), Jarvis636431 (#1499), and leninworld, whelanh,
  bryanbans, YongxinFu and ChenShuo2004 (#1495). The CLA check no longer deadlocks on a contributor's own
  self-signature PR (PR [#1432](https://github.com/xerj-org/xerj/pull/1432),
  carries todie's #1404, with the inputs pinned to the event SHA).

### Housekeeping

- Dead-code and comment cleanups: PRs #1447, #1448, #1449, #1453, #1454.
- CI and test reliability: PR #1422 (fjjjuv, transport auth tests retry a
  port bind race); PR #1490 (the fuzz workspace's lockfile is seeded from
  `engine/Cargo.lock`, because every `zip` 8.x release was yanked upstream
  and a fresh resolve failed); PR #1494 adds a manual workflow that prints
  the public half of the hub signing seed. It bakes no key into the binary:
  `HUB_PUBKEY_HEX` is still empty in rc.96. PR
  [#1506](https://github.com/xerj-org/xerj/pull/1506) repairs two merge skews
  that made main red during the cut: the `auto_port` tests (#1487) against
  #1452's `Result`-returning argument parser, and a comma the #1499 merge
  dropped from `.contributors`.

<!-- notes-exempt: #1347 (ops: the xerj.org metrics Cloudflare Worker; ships nothing in the binary) -->

## [1.0.0-rc.95] - 2026-10-12

Window since v1.0.0-rc.94: legacy Microsoft Office support in autoindex,
hardened against hostile input.

### Added

- **Legacy Microsoft Office formats in `xerj autoindex`** (PR
  [#1324](https://github.com/xerj-org/xerj/pull/1324)) — OLE2 Compound File
  attachments, detected by content (never by extension), are now indexed
  instead of dropped as binary:
  - **`.xls`** (BIFF, via `calamine`) — one dataset per worksheet, one typed
    record per row, mirroring the `.xlsx` extractor.
  - **`.doc`** (Word 97+) — document text recovered through the FIB piece
    table, titled from `SummaryInformation`. Text-only by design: no
    formatting, tables-as-rows, headers/footers, footnotes, or pre-Word-97
    binaries (those fall back to a lossy read or are skipped).
  - Other OLE2 containers (e.g. `.epf` 1C data-processors) are **named and
    skipped**, not mis-parsed.

### Security

- **`.xls` parsing is process-isolated** (PR
  [#1324](https://github.com/xerj-org/xerj/pull/1324)) — `calamine` 0.26.1 is
  not panic-safe on hostile BIFF (crafted `DIMENSIONS`/`SST` records force
  multi-terabyte allocations; malformed `Lbl`/`ExternSheet` records panic), and
  the engine is `panic=abort`, so a single hostile spreadsheet in an untrusted
  dump could abort the whole indexer. The BIFF read now runs in a short-lived
  `xerj __extract-xls` worker under `RLIMIT_AS`/`RLIMIT_CPU` + a wall-clock
  watchdog + process-group kill; the parent reads records back as NDJSON and
  degrades any worker crash/timeout to a skipped file, never calling `calamine`
  on the raw bytes. A `biff_allocations_bounded` pre-filter rejects the known
  over-allocation shapes before the spawn. Adversarially verified: the indexer
  survives every crafted vector while benign workbooks still extract. (Windows
  keeps the in-process pre-filter as a documented best-effort.)

### Housekeeping

- Landing footer version stamps swept to rc.94 (PR
  [#1325](https://github.com/xerj-org/xerj/pull/1325)); the sweep is now part
  of the release-cut recipe.

### Known gaps

- OpenDocument `.ods`/`.odt` (zip-based) are detected and **skipped** (no
  extractor yet); `.rtf` indexes as raw text. Tracked for a follow-up.

## [1.0.0-rc.94] - 2026-10-11

Window since v1.0.0-rc.93: a wave of external-contributor fixes landed
alongside a backlog-fix sweep. Contributors in this window: Vinz2168,
thomas-villani (Tom Villani), yogthos, and pierreaubert.

### Fixed

- **Hybrid `query`+`knn` kept a page-local ranking on delete-bearing indexes**
  (issue [#1303](https://github.com/xerj-org/xerj/issues/1303), PR
  [#1312](https://github.com/xerj-org/xerj/pull/1312)) — a single historical
  overwrite/delete flipped the pinned-kNN compound tree off the exact
  index-wide BM25 path onto the stored-doc page heuristic, so the lexical half
  scored against a page sample instead of corpus-wide document frequency:
  correct on a handful of docs, unrelated at scale. The `!deletes_present`
  gate on the pinned-kNN FTS projection is removed; the append-only path is
  byte-identical, with a regression test that uses enough documents that
  page-local and corpus-wide statistics diverge.
- **Bulk auto-id loads rejected the whole batch for one lone UTF-16 surrogate**
  (issue [#1304](https://github.com/xerj-org/xerj/issues/1304), PR
  [#1311](https://github.com/xerj-org/xerj/pull/1311)) — the turbo index path
  validated each line with `serde_json` `IgnoredAny`, which does not decode
  `\uXXXX` escapes, so a lone surrogate slipped through to the deferred
  whole-batch `Value` parse and failed every item. The bad document is now
  rejected per-item with a 400; the rest of the batch lands.
- **autoindex `--watch` allowed two live owners of one journal/destination**
  (issue [#1232](https://github.com/xerj-org/xerj/issues/1232), PR
  [#1307](https://github.com/xerj-org/xerj/pull/1307)) — a session now holds a
  session-long exclusive lock (`.autoindex.watch.lock`); a second watcher for
  the same state dir is refused with a bounded, non-zero-exit message.
- **autoindex incremental `--no-graph` reconcile aborted on sibling forks**
  (issue [#1230](https://github.com/xerj-org/xerj/issues/1230), PR
  [#1309](https://github.com/xerj-org/xerj/pull/1309)) — the frozen plan never
  persisted the per-repository scope genesis clustering uses, so byte-identical
  sibling repos could not be told apart; the scope is now persisted.
- **Second Brain console rendered a notes/beliefs persona on non-note corpora**
  (issue [#1100](https://github.com/xerj-org/xerj/issues/1100), PR
  [#1310](https://github.com/xerj-org/xerj/pull/1310)) — the console now picks a
  neutral records/active frame vs the notes persona from the real `ax_format`
  tally, so autoindexing a code tree no longer frames it as personal notes.
- **memtable `match`/`match_phrase` tokenized with the wrong analyzer**
  (issue [#1280](https://github.com/xerj-org/xerj/issues/1280), PR
  [#1295](https://github.com/xerj-org/xerj/pull/1295), thomas-villani) — the
  memtable now tokenizes with the analyzer the segment uses, so hit sets are
  stable across a flush.
- **`query_string` field groups and negation** (issues
  [#1298](https://github.com/xerj-org/xerj/issues/1298),
  [#1299](https://github.com/xerj-org/xerj/issues/1299), PRs
  [#1300](https://github.com/xerj-org/xerj/pull/1300),
  [#1301](https://github.com/xerj-org/xerj/pull/1301), Vinz2168) —
  `field:( … )` groups are scoped to the field and the classic parser builds
  one boolean per level with correct occur semantics.
- **A non-numeric value on a numeric field is a 400, honouring `lenient`**
  (issue [#1284](https://github.com/xerj-org/xerj/issues/1284), PR
  [#1293](https://github.com/xerj-org/xerj/pull/1293), Vinz2168).
- **`highlight.fields` expands field-name patterns** (issue
  [#1281](https://github.com/xerj-org/xerj/issues/1281), PR
  [#1294](https://github.com/xerj-org/xerj/pull/1294), Vinz2168).
- **`_msearch` reports the envelope-level `took`** (issue
  [#1288](https://github.com/xerj-org/xerj/issues/1288), PR
  [#1289](https://github.com/xerj-org/xerj/pull/1289), Vinz2168).
- **`terms` order resolves percentile keys and rejects unknown paths** (issues
  [#1282](https://github.com/xerj-org/xerj/issues/1282),
  [#1283](https://github.com/xerj-org/xerj/issues/1283), PR
  [#1290](https://github.com/xerj-org/xerj/pull/1290), Vinz2168).
- **No empty `Warning: 299` header on every response** (issue
  [#1285](https://github.com/xerj-org/xerj/issues/1285), PR
  [#1291](https://github.com/xerj-org/xerj/pull/1291), Vinz2168).
- **`embedding.timeout_ms` reaches the proxy client in milliseconds** (PR
  [#1276](https://github.com/xerj-org/xerj/pull/1276), thomas-villani).
- **`index.max_result_window` above 10,000 works; `limits.max_result_window`
  is the node default** (PR
  [#1277](https://github.com/xerj-org/xerj/pull/1277), thomas-villani).
- **autoindex CLI argument hygiene** — `xerj def --kind/--lang` still requires
  the name to match (PR [#1272](https://github.com/xerj-org/xerj/pull/1272)),
  each watch route refuses flags it cannot read (PR
  [#1273](https://github.com/xerj-org/xerj/pull/1273)), the legacy-journal
  rebuild argv carries every index-shaping flag (PR
  [#1274](https://github.com/xerj-org/xerj/pull/1274)), and map-only
  `--dataset` is refused outside `autoindex map` (issue
  [#1231](https://github.com/xerj-org/xerj/issues/1231), PR
  [#1308](https://github.com/xerj-org/xerj/pull/1308)). thomas-villani / xerj-org.
- **Row-exact fast aggregations under resident ghosts** (issue
  [#1267](https://github.com/xerj-org/xerj/issues/1267), PR
  [#1270](https://github.com/xerj-org/xerj/pull/1270)) — bounds the 803 s
  fast-agg finalize-verify window. Partial: #1267 **stays open** for the
  finalize-catalog read-back walk and the `size > 0` doc-lane residual,
  which still brute-scan.

### Added

- **autoindex AST extraction for Clojure and Scheme** (PR
  [#1305](https://github.com/xerj-org/xerj/pull/1305), yogthos).

### Changed

- **Dormant config keys named at startup** (PR
  [#1275](https://github.com/xerj-org/xerj/pull/1275), thomas-villani) — every
  accepted-but-unread key is announced as DORMANT at startup and no longer
  documented as working.

### Docs

- Config/cluster/env/WAL docs describe the behavior the binary actually has
  (PR [#1278](https://github.com/xerj-org/xerj/pull/1278), thomas-villani);
  `--help` names the CORS config and the real native-port default (issue
  [#1286](https://github.com/xerj-org/xerj/issues/1286), PR
  [#1292](https://github.com/xerj-org/xerj/pull/1292), Vinz2168). Field reports
  against live ES 8.13.4 and a reference-coding corpus index landed from
  Vinz2168 (PRs [#1287](https://github.com/xerj-org/xerj/pull/1287),
  [#1302](https://github.com/xerj-org/xerj/pull/1302)) and pierreaubert (PR
  [#1297](https://github.com/xerj-org/xerj/pull/1297)).

### Housekeeping

- Landing footer/version-stamp sweep so the `Landing constants` guard on
  `main` is green (PR [#1271](https://github.com/xerj-org/xerj/pull/1271)), and
  the yogthos CLA signature (PR
  [#1306](https://github.com/xerj-org/xerj/pull/1306)).
- Closed as already-fixed on `main` after verification: the sorted-scan
  liveness bug [#1212](https://github.com/xerj-org/xerj/issues/1212) (via
  #1013), the raw-JSON own-field recall gap
  [#1244](https://github.com/xerj-org/xerj/issues/1244) (via #1246/#1251), and
  the corpus-index resume honesty bug
  [#1173](https://github.com/xerj-org/xerj/issues/1173) (via #1174).

## [1.0.0-rc.93] - 2026-10-08

### Fixed

- **The autoindex finalize-verify probe fell off the columnar fast path**
  (issue [#1260](https://github.com/xerj-org/xerj/issues/1260), PR
  [#1262](https://github.com/xerj-org/xerj/pull/1262)) — two walls sat between
  the verify query shape (`bool.filter [terms …, exists <field>]`) and the fast
  path: `exists` had no arm in the predicate compiler, and the deletes bail
  keyed on the MONOTONIC `ghost_events` counter, so an index that had ever
  seen an overwrite — the catalog is rewritten by every finalize-catalog —
  never re-qualified, even after merges compacted every ghost away. `exists`
  now compiles to a columnar predicate and admission is per-segment via the
  ghost-position bitmap: empty bitmaps proceed (live == physical), dirty ones
  still bail. Measured on the live 402,814-doc autoindex-catalog crawl node:
  the per-digest probe ran 3.4 s on the pre-fix server and 2.7 ms warm on the
  fixed one (~1,250×; the first post-restart probe pays an 8.8 s cold page-in,
  verified to be one-time by a warm repeat). New integration file
  `fast_aggs_exists_and_deletes.rs`: six tests pinning fast-path/brute
  agreement across present / JSON-null / absent / empty-string exists shapes,
  the meta-field bail, the unmerged-ghost bail, merged-history re-qualify,
  and object-valued memtable docs. The served/bailed assertions read the
  process-global `FAST_PATH_AGGS_SERVED` counter as a before/after delta, and
  CI run 37816544794 caught the six tests racing on it — the file now
  serializes behind one tokio Mutex (the engine half was never implicated:
  serialized, the meta-field query bails exactly as required).

- **`pack publish` could not survive a same-day rebuild under immutable
  releases** (PR [#1268](https://github.com/xerj-org/xerj/pull/1268)) — the
  re-run path deleted today's PUBLISHED pack release and recreated the same
  tag, but a burned tag can never name another release (run 37826006767 hit
  the exact 36358399114 error pair and left the day without a published
  rust-vulns pack; repaired by hand under `-r2`). Both pack jobs now pick the
  first unused tag (date, then `-r2`, `-r3`, … by release + git-ref probe),
  retag past an immutable-release refusal instead of failing, and never
  delete a published release for a rebuild. The retention step also finally
  works: `gh release list --json` was an unsupported flag, so it had silently
  matched zero releases since M6 — it now sorts by `created_at` via the API
  (a `-r2` suffix breaks lexicographic tag order) and includes abandoned
  drafts.

### Performance

- **The finalize-verify barrier read back one changed group at a time — three
  serial searches per file** (PR
  [#1259](https://github.com/xerj-org/xerj/pull/1259), the #1183 count lane)
  — cve-records projected ~50 h and vuln-fix-commits ~120 h of verify tail
  against index phases measured in hours. Verify now batches per (index ×
  1,024 content digests): record windows (terms filter + exact terms
  aggregation) run 63 ms cold / 6 ms warm on the columnar fast path, the
  catalog leg `_source`-restricted paged fetch-count runs 31 ms cold / 14 ms
  warm, delete-aware, with mid-window total changes and count/total
  mismatches failing loud. ~1.2 M serial searches collapse to ~800 windows
  for cve-records. Failure messages and the first-disagreement group are
  unchanged; the progress denominator is now windows, so the line never sits
  at `0/N` for a walk's duration. `cargo test -p xerj-autoindex --lib`:
  1299 passed, 0 failed. First live proof: the vuln-fix-commits rebuild
  completed on this client — 328,891 records, 97 batched windows verified.

### Added

- **`agent-session-trajectories`, the first external-contributor corpus pack**
  (PRs [#1263](https://github.com/xerj-org/xerj/pull/1263),
  [#1265](https://github.com/xerj-org/xerj/pull/1265); recipe and stats on
  corpus-hub in #1255 and #1264) — 70 sanitized multi-session agent
  trajectories from chunxiaoxx / Nautilus (demand anchored to #1138, wishlist
  slot #1210), CC-BY-4.0, 70 envelopes → 70 records with failure turns kept
  verbatim. The pack ships with its own ed25519 keypair: `.pub` committed at
  `tools/packs/keys/agent-session-trajectories.pub`, the seed as the
  `PACK_SIGNING_SEED_AST` Actions secret, and a dedicated job in the
  scheduled `pack publish` workflow — build, sign, consumer-path verify
  against the committed key, dated immutable release. First release
  `pack-agent-session-trajectories-2026-10-08` published and consumer-
  verified from the public release URL (70 records, signature checked against
  the `raw.githubusercontent.com` key).
- **`vuln-fix-commits` live on the hub with graded evidence** (corpus-hub PR
  #1266) — project-kb fix commits: 328,891 records, 97 batched verify windows
  passed, and the pre-registered G7 suite blind-graded 4/5 PASS (three
  rank-1 needle hits, one rank-3, one miss where the 2020-era sleuthkit
  `yaffsfs_istat` payload lost to newer buffer-overflow payloads on generic
  terms). Graded evidence committed at
  `tools/xerj-code/hub/backlog/g7-vuln-fix-commits-2026-10-08-graded.json`.
  The hub stands at 103 manifests / 105 live rows.

## [1.0.0-rc.92] - 2026-10-08

### Fixed

- **A corpus whose records carry prose in schema-named fields was invisible to
  `xerj code`** (issue
  [#1244](https://github.com/xerj-org/xerj/issues/1244), PR
  [#1246](https://github.com/xerj-org/xerj/pull/1246)) — the pre-registered
  g7-cve-records-2026-10-08 suite measured 0/7 because `resolve_fields` sent
  only the synthesized `text^0.5`: one shard mapped `text` and its presence
  kept the own-field fallback from ever firing, while the records' actual prose
  lived in fields like `containers_cna_descriptions` (rank 1 at 18.76 queried
  directly). Own text fields now join the `multi_match` as `^0.5` recall legs
  beside the standard fields — safe because `best_fields` executes as dis_max,
  so a leg can only win by genuinely outranking the content fields at half
  weight — gated to fields mapped by at least a quarter of the union's indices.
  Measured spectrum the gate sits on has an empty middle: schema prose repeats
  across shards (cve-records 37-96% of 118 indices) while README-frontmatter
  junk does not (exploit-pocs-2026: 2,143 own text fields, all ≤1% — the old
  alphabetical 24-cap kept a literal `$comment` and cut `Summary`). The
  mechanism fix is complete; #1244 itself stays open until the
  pre-registered G7 suites pass on the rebuilt corpora — the cve-records
  and vuln-fix-commits rebuilds were in flight at release time.

- **A multi-family corpus still lost its minority family to that same gate**
  (issue [#1244](https://github.com/xerj-org/xerj/issues/1244), PR
  [#1251](https://github.com/xerj-org/xerj/pull/1251)) — vuln-fix-commits is
  project-kb repo source (body/defs/title) beside 29,240 fix-commit payloads
  whose prose lives in `message`, mapped by 4 of 26 union indices against a
  gate demanding 7: the corpus answered every query from its tooling files
  while every payload was unsearchable (the needle was rank 1 at 36.35 queried
  on `message` directly, zero payload hits through the client). A field now
  joins when it passes the quarter gate OR any index that maps none of the
  standard content fields maps it, and the 24-leg cap ranks by mapping coverage
  instead of name. Exact-signature family grouping was measured and rejected:
  the mapper types only fields present in a shard's documents, so one family
  splits into `message,patch,text` and `files,message,text` shards and each
  half fails its own majority rule. Regression-checked on exploit-pocs-2026
  (pinned query "MCPJam inspector 23744": needle README still rank 1, POC.py
  rank 3), whose 689 own-only indices (12% of the corpus) are newly reachable.
  #1244 stays open for the same completion condition as above: the suite
  reruns on the rebuilt corpora close it, not this code.

- **A corpus whose primary content rides the plain-text family was invisible
  to `xerj code`** (issue
  [#1254](https://github.com/xerj-org/xerj/issues/1254), PR
  [#1258](https://github.com/xerj-org/xerj/pull/1258)) — the `text^0.5`
  recall-leg weight is the #1238 calibration for corpora where the text
  family carries mirror noise beside code-family `body` records, but
  otel-proto's primary content IS the text family (every `.proto` definition
  is a txt-lines record), so the discount hid the corpus's point: the
  pre-registered G7 suite's four proto needles ranked 19/absent/39/23 behind
  semantic-conventions prose. Two earlier mechanisms were measured and
  disproven first — absence (a filename wildcard on `ax_file`, which holds
  content digests; the correct `ax_path` probe shows all 79 records) and
  txt-lines chunk-splitting (a simulated whole-document route ranked the
  needle 25th, worse than the real chunks' 19th: length dilution beats mass
  concentration). A corpus may now declare `query.text_weight` in its
  `corpus.json` (bounds 0.25..=4.0 enforced; outside them the client warns
  and keeps the 0.5 default), hub pins carry the block through
  `corpus add --from`, and the query path applies it to the `text^` slot.
  Measured on the unchanged index bytes: all four needles 19/absent/39/23 →
  rank 1, the suite re-grades 5/5 against a ≥3/5 bar, and the no-hint path
  is byte-identical old vs new binary (diffed on cisa and
  exploit-pocs-2026). #1254 stays open only for the minor
  `code_files=0` labeling gap in the index terminal line.

<!-- notes-exempt: #1252 (changelog-only carry of the rc.92 entries; no code,
     cites the window PRs #1246 and #1251 itself) -->

## [1.0.0-rc.91] - 2026-10-08

### Added

- **The exploit-hub corpus, tested on three post-pin CVEs with and without it** (PR
  [#1240](https://github.com/xerj-org/xerj/pull/1240)) — the A/B eval write-up the
  cyber-exploits group was built for: one analyst, two arms (plain Claude Code vs
  Claude Code + the corpus), three CVEs published after the corpus pin, sealed ground
  truth. On the needle (CVE-2026-23744, 36 indexed PoC repos) one `multi_match` round
  over 1,093,749 docs named `POST /api/mcp/connect` and `serverConfig.command` and the
  one-request trigger, cited — the advisory alone could not; both arms then predicted
  the wrong patch and that loss stays in the table. On the analogy (CVE-2026-105844,
  no PoC anywhere) Exploit-DB entry 52528 supplied the denylist segments and both
  mechanisms the sealed 3.88.0 diff shipped. The empty stratum (CVE-2026-97332) is
  counted, not dropped. The negative control too: 23 s retrieval vs 2.1 s clone+grep
  when repo names are already known — the corpus is the discovery layer, not a fetch
  accelerator. n=3, one analyst: a case study with measured arms, not statistics, and
  the post says so. The same PR caught live client defect
  [#1238](https://github.com/xerj-org/xerj/issues/1238) during the eval. Also ships
  the `xerj-blogposts` house-writing corpus at 8 records (this post included; the
  registry pin moves with the rebuild in
  [#1242](https://github.com/xerj-org/xerj/pull/1242)).

### Fixed

- **`exists` inside a filter-context `bool` had no arm in the columnar filter
  executor, so the whole plan bailed and source-scanned every row** (issue
  [#1183](https://github.com/xerj-org/xerj/issues/1183), PR
  [#1234](https://github.com/xerj-org/xerj/pull/1234)) — keyword `exists` lowers to
  an empty-prefix dictionary range, numeric/boolean to an unbounded window (the same
  lowerings the standalone root `exists` uses), and text/`semantic_text` fields get a
  new source-backed `SourceExists` leaf that pays one stored-source parse per term
  survivor instead of per row. Meta fields stay on the brute path. Measured on the
  standing node's 91,143-doc segment: the conjunction fell 9,638 ms -> handled by the
  plan. The finalize-verify crawl itself is larger than this fix: the count-context
  gate still excludes `SourceExists` plans and a single tombstone disables the
  term-count fast path, so the exact-semantic-count shape on the 572,992-doc index
  still brute-scans — measured 2026-10-08 and left tracked in #1183, which stays open
  until a full xerj-search rebuild passes finalize with verified numbers.

- **One sibling repo could fill a whole `xerj code` page, and the plain-text
  extraction family could outscore code-family hits** (issue
  [#1238](https://github.com/xerj-org/xerj/issues/1238), PR
  [#1241](https://github.com/xerj-org/xerj/pull/1241)) — two diversification fixes
  in `xccode`. A per-source-INDEX cap (`MAX_PER_INDEX = 1`, engaged only when the
  fan-out has at least as many distinct indices as the page wants, so a homogeneous
  corpus's handful of shard indices is untouched): measured on the live exploit group
  (query "MCPJam inspector 23744", 5,644 indices, 36 needle PoC repos), one
  sibling-CVE demo repo took 6 of the top 10 slots with a different file each — the
  per-FILE cap of #1137 cannot see that wall — and the capped page held 5 needle
  records where the old page held 1. And `text` joins the `multi_match` as a
  `^0.5` recall leg (same posture as `defs_expanded^0.5`): at full weight one
  plain-text demo index outscored every code-family hit; at `^0.5` the family stays
  searchable (same 39 total hits) while needle docs keep their scores.

## [1.0.0-rc.90] - 2026-10-07

### Added

- **A second community contribution from Vinz2168: the agent field report for the #1091
  diagnosis session** (PR
  [#1219](https://github.com/xerj-org/xerj/pull/1219)) — one report under
  `user-feedback/16-agent-field-reports/`, written by the AI coding agent that ran the session
  and reviewed by its operator, alongside the #1217 fix that session produced (rc.89). Every
  number in it comes from a command run in that session (the A/B harness and the ES-YAML runner
  output quoted in #1217). Filing it closed the tracker
  [#1091](https://github.com/xerj-org/xerj/issues/1091) it diagnoses.

### Fixed

- **The kNN exact scan could silently drop a document it had already captured** (issue
  [#1220](https://github.com/xerj-org/xerj/issues/1220), PR
  [#1223](https://github.com/xerj-org/xerj/pull/1223)) — two holes, both fixed. The scan read
  the memtable and the store snapshot with no generation check between them, so a concurrent
  flush or merge publication could land mid-capture and the "captured" set was half old
  generation, half new; and the admission predicate consulted the *live* version map, so a
  post-capture update of the same id made the captured copy look stale and the scan forgot it.
  Both walkers now bracket the capture (generation-checked) and admit candidates against the
  capture-scoped view, not the live one.

- **A no-match `delete_by_query` / `update_by_query` run no longer flushes the index** (issue
  [#1222](https://github.com/xerj-org/xerj/issues/1222), PR
  [#1227](https://github.com/xerj-org/xerj/pull/1227)) — by-query runs flushed the member
  index FIRST, unconditionally, as the #1019 paging precondition; a run whose query matched
  nothing still paid it. A `size:0` probe through the ordinary search path now runs first and a
  zero-match returns the zero response without flushing; a matching unflushed doc still counts
  and takes the flush arm exactly as before. This also corrects the #1222 root cause on
  record: the idle-flush fingerprint loop was already dead (`Index::flush()` clears the
  idle-probe pair since #878) — the measured "6,848 one-doc segments for ~3,005 writes" was
  this unconditional flush firing under the corpus apply loop's per-op defensive deletes.
  Live A/B on a throwaway node, one doc written with `refresh=false`: old binary, no-match
  delete → 1-doc segment created; this build → 0 segments, and a matching delete still
  deletes (`total 1 / deleted 1 / batches 1`).

- **The corpus apply loop asks before deleting: the per-group defensive `delete_by_query` is
  gated on a visibility probe** (one leg of
  [#1224](https://github.com/xerj-org/xerj/issues/1224), PR
  [#1225](https://github.com/xerj-org/xerj/pull/1225)) — every upsert used to fire a
  defensive `delete_by_query?refresh=true` even when the index held nothing of that content,
  and one-record-per-file corpora pay one upsert per FILE: cvelistV5's cve-records build is
  402,744 groups, i.e. 402,744 no-match deletes at a measured 5–16 s each under concurrent
  bulk load (the forced `refresh=true`), against a `size:0` term probe that answers in <1 ms.
  The delete now fires only when the probe sees something.
  #1224 stays open pending the end-to-end re-measure: the O(1) journal fix below removed the
  measured client-CPU ceiling, and the tracker closes on the completed apply-run numbers, not
  on the individual legs.

### Performance

- **Corpus apply: O(1) journal state writes and per-replay lookups** (the client-CPU leg of
  [#1224](https://github.com/xerj-org/xerj/issues/1224), PR
  [#1228](https://github.com/xerj-org/xerj/pull/1228)) — `Journal::sync_operation_state` ran
  twice per replayed operation and cloned the entire `PendingSync` (the desired manifest with
  all 402,695 groups, the 402,744-entry operations Vec, the growing operation_states map) to
  append one small durable line: ~350 ms × 2 per file, measured on cve-records as apply at
  **1.18 items/s** with 0.72 s client CPU per file, one core 82% user, RSS 5.7 GB. State
  writes are now validate → durable append → record in place (the journal line stays the
  transaction boundary); `PendingSync` carries a lazily-built membership index; the replay
  path resolves groups and artifacts through a once-per-replay lookup table instead of
  O(402k) scans. `xerj-autoindex` lib suite 1299 passed / 0 failed; journal failpoint suites
  unchanged (same events, same order). The after-rate posts on
  [#1224](https://github.com/xerj-org/xerj/issues/1224) when the in-flight re-measure
  completes.

## [1.0.0-rc.89] - 2026-10-07

### Added

- **A guest post on the blog: "Six eval scars from a production agent-memory system"** (PR
  [#1211](https://github.com/xerj-org/xerj/pull/1211)) — the community contribution from Chunxiao Wang
  (Assay / nautilus-compass), landed as a guest post with every measurement his and attributed: topic
  slicing beating token windows on recall@1 by double digits, an MTEB-leading embedder losing his
  embedder bakeoff on recall@1, qid grouping against leakage, three-valued retrieval judged with
  Brier 0.069, cached-input variance attribution, and the paired-vs-unpaired overstatement (a 4×
  claim collapsing under a paired re-run against an always-yes baseline). The XERJ-side section
  answers the question the post asks back, checked rather than guessed: 0 of 99 live hub corpora and
  0 of 132 backlog rows are session or trajectory data, our chunk unit is file/section, and the
  memory features store curated facts retrieved by BM25 — so the eval shapes the post scars describe
  are not yet measurable on this engine, which is the honest answer.

### Performance

- **Exact semantic/kNN scans on passage-chunked corpora no longer re-parse every stored
  document per query** (issue
  [#1091](https://github.com/xerj-org/xerj/issues/1091), PR
  [#1217](https://github.com/xerj-org/xerj/pull/1217)) — a
  `semantic_text` field whose documents carry passage vectors stays on the exact scan by design
  (309f0b6f), and that scan parsed each segment's stored documents (vectors included, ~53 KB a
  document on FiQA) to read two fields per candidate; they overflowed the hydration budget, the
  RSS spike tripped the memory-watermark drain, and every query paid the whole decode again. An
  unfiltered scan now ranks from per-segment vector columns that also carry `_id`/`_seq_no`,
  builds a missing column from the stored section's typed kNN projection (no `_source`
  reconstructed), and hydrates only the ranked winners row-selectively — without the generated
  embedding companions when the default `_source` would drop them anyway. Answers are unchanged
  (bit-identical scores against the existing clone-everything oracle; 50/50 identical ranked
  ids on FiQA). FiQA, 57,638 docs, lexical embedder, same machine: `semantic` p50 3,865 →
  261 ms, `hybrid` p50 5,772 → 289 ms, memory-watermark crossings 25 → 0
  (`benchmarks/beir-hybrid/results/2026-10-07-issue-1091/`). Filtered and `scalar8` scans keep
  the stored-document path; a segment stored as LZ4 (under 128 documents) still builds its column
  and hydrates its winners from parsed documents. The issue stays open: this is the latency half
  under the lexical embedder — the 13-26 s readings were taken under `--embed-mode neural` on a
  loaded box, not re-measured here, and the capture-scoping follow-up the review surfaced is
  separately tracked in [#1220](https://github.com/xerj-org/xerj/issues/1220).

### Fixed

- **The autoindex page walkers no longer read a timed-out partial page as end-of-data** (part of issue
  [#1212](https://github.com/xerj-org/xerj/issues/1212), PR
  [#1213](https://github.com/xerj-org/xerj/pull/1213)) — the engine applies a 30 s cooperative deadline
  to any search whose body carries no timeout and answers a missed deadline with HTTP 200, PARTIAL hits
  and `timed_out: true`, which is legal on the wire; the xerj-search finalize read-back walked into one
  on a breaker-drained node (a sorted page answered 441 hits with the flag set) and
  `catalog_generation`'s short-page break read it as the last page — 56,441 of 58,568 documents
  certified. `Es::search_page` now sends every page with an explicit 120 s timeout (added only when the
  caller set none; above the slowest sorted page measured on that node) and retries the SAME page,
  bounded at six attempts, then fails loudly naming the index; all three walkers
  (`catalog_generation`, `detect::scan_by_id`, the `delete_catalog_hits_where` sweep) use it. The
  engine-side question — whether the 30 s default should stand — stays open on
  [#1212](https://github.com/xerj-org/xerj/issues/1212).
- **`xerj corpus index` refuses a run it cannot resume over a recorded build** (issue
  [#1214](https://github.com/xerj-org/xerj/issues/1214), PR
  [#1215](https://github.com/xerj-org/xerj/pull/1215)) — a state file that records a build pins the
  corpus to one url, one prefix and one state dir, but a failed conjunct (observed live: `localhost`
  vs the recorded `127.0.0.1`) fell through to legacy mode silently, and autoindex then opened its
  default hash-of-root journal — a DIFFERENT journal holding a stale pending sync from a retired
  attempt — which began re-applying 46,308 operations into the legacy namespace beside the recorded,
  still-unverified build. Because `xerj code` reads the corpus namespace as one wildcard, both
  generations served and every hit was duplicated across incompatible groupings. Any failed conjunct
  now exits 2, printing each recorded value that did not hold and the two recovery routes (matching
  `--url` to resume; `--fresh` to rebuild beside it); no autoindex invocation runs from the refusal
  path.
- **The finalize catalog read-back re-walks while it falls short of the projection** (part of issue
  [#1212](https://github.com/xerj-org/xerj/issues/1212), PR
  [#1216](https://github.com/xerj-org/xerj/pull/1216)) — the standing rebuild's read-back walk read 9
  full 1,000-hit pages, then an EMPTY page with `timed_out` absent: 9,000 of 58,568 documents, no
  flag, no error, while a terms aggregation on the same field counted all 58,568 and the same walk on
  an idle node minutes later read all 59 pages clean. Neither existing defense can see that shape (a
  5xx retry only re-sends failed requests — this answer was 200; the #1213 page check keys on
  `timed_out` — the flag was absent; page answers report `total {relation: "gte"}` under the
  track_total_hits cap, so total cannot validate a page either). The one signal that distinguishes a
  starved walk from a complete one is observed-count-versus-expected, which the barrier already
  computes: `catalog_generation_complete()` now refreshes and re-walks while the observation is short
  of the sealed projection's document count, bounded at three attempts, and returns the LAST
  observation when all fall short so `validate_observed` still produces its loud, precise mismatch.
  The #971 prior-run stray sweep is deliberately not re-walked: it is a subset check, an unflagged
  truncated page can only under-report strays, and a false pass is impossible. The engine-side truncation itself
  is fixed in this release by PR
  [#1218](https://github.com/xerj-org/xerj/pull/1218);
  [#1212](https://github.com/xerj-org/xerj/issues/1212) stays open until the
  standing xerj-search rebuild verifies against the released fix.
- **Scans no longer drop their captured hits when a merge or flush publishes mid-walk** (part of issue
  [#1212](https://github.com/xerj-org/xerj/issues/1212), PR
  [#1218](https://github.com/xerj-org/xerj/pull/1218)) — the #1013 capture bracket validates a
  writer-free instant and the scan then runs on frozen state (segment snapshot, read lease, captured
  memtable), but three per-hit liveness checks read the LIVE version map and dropped any hit whose
  entry no longer named its own segment: a merge publishing after the capture repoints surviving docs
  at the merged segment (`snapshot.rcu` + version-repoints), a flush repoints off `__memtable__`, and
  the frozen hits then failed the equality check and were silently dropped — a short or empty page,
  HTTP 200, `timed_out` absent, self-healing on the next capture. Exactly the read-back shape the
  client legs above defend around, now fixed at the source. A scan may only apply version entries
  that describe the world it captured (`CapturedEntryVerdict`): an entry on `__memtable__` or a
  captured segment decides live/invisible as before; an entry naming a real segment outside the
  captured set is post-capture and invisible in both directions. The design follows tantivy's
  reader-generation pinning (SearcherInner behind an ArcSwap swap, segment readers under META_LOCK,
  live generations pinned via Inventory — the snapshot as the only scan authority, adapted not
  copied). Park-hook regression tests park a scan between capture and walk across a merge and a flush
  publication; both lost their captured hits before the fix. The memtable ghost filter, the scored
  walk and the fused-bool residual walk are all capture-scoped now.
  [#1212](https://github.com/xerj-org/xerj/issues/1212) stays open until this ships in a release and
  the standing xerj-search rebuild completes against it; the kNN exact scan's admission predicate
  still reads the live map (a pre-existing predicate #1217 extracted into a helper, window ~15×
  narrower for it) and is separately tracked in
  [#1220](https://github.com/xerj-org/xerj/issues/1220).

## [1.0.0-rc.88] - 2026-10-07

### Added

- **`xerj autoindex --code-analyzer standard|code`** (issue
  [#1198](https://github.com/xerj-org/xerj/issues/1198), landed as PR
  [#1205](https://github.com/xerj-org/xerj/pull/1205)) — `code` declares the engine's built-in
  identifier-aware analyzer (standard tokenizer + word-delimiter + lowercase: `snake_case`,
  `camelCase` and dotted identifiers gain their sub-words, the original token is kept) on newly
  created datasets that hold at least one source file, so a query that says "connection pool"
  reaches `get_connection_pool` through BM25. Default `standard` keeps today's election
  unchanged. With `code` elected the #1059 stemmer defers for that dataset. It rides the #1059
  create-body path: committed `index_identity` digests do not move and existing indexes keep
  their analyzer. Measured on SWE-bench Lite file localization: acc@5 +0.11 [+0.03, +0.19] on a
  pre-registered held-out sample; cost ~15% index size and ~2.4× text-analysis time.
- **`_cat/indices` and `_cat/segments` accept `h`, `v` and `bytes`** (issue
  [#1201](https://github.com/xerj-org/xerj/issues/1201), PR
  [#1208](https://github.com/xerj-org/xerj/pull/1208)) — all three output parameters were
  silently ignored on both endpoints. A shared renderer now serves them the way ES does:
  `h` selects comma-split columns (`*` expands, unknown names drop out, and a `format=json`
  object carries exactly the selected columns as strings), `v` turns on text padding —
  which is the *only* mode that pads, a plain response joins columns with one space and
  still prints the header over zero rows — and `bytes` resizes the byte columns, where
  `b` is raw and the one-letter units are decimal (10³) while the two-letter ones are
  binary (2¹⁰); an unknown value for either parameter is a 400 that names the valid set
  rather than a silently ignored flag.

### Fixed

- **`_cat/segments` returns real segment rows** (issue
  [#1202](https://github.com/xerj-org/xerj/issues/1202), PR
  [#1208](https://github.com/xerj-org/xerj/pull/1208)) — the endpoint used to answer
  405/empty. It now reads per-segment truth from the store: id, physical `docs.count`,
  `docs.deleted` decoded from the segment's own ZTB2 tombstone section, size, and the
  remaining ES columns, over a 14-column ES-compatible header, including the
  whole-cluster `/_cat/segments` form. The per-segment counts follow the store's real
  geometry, pinned by a test: a delete's tombstone persists in a *later* segment than the
  document it kills, so a segment's `docs.deleted` is the tombstones persisted in that
  segment and the live count is the sum invariant `Σ docs.count − Σ docs.deleted` — the
  same pre-merge over-count `_cat/indices` has always had for superseded update copies,
  now visible and documented instead of guessed at. Row count matches `_stats`
  `segments.count` (also pinned).
- **The rc.87 landing sweep is on main, and the landing constants guard is green again** (PR
  [#1206](https://github.com/xerj-org/xerj/pull/1206)) — the rc.87 roll stamped the engine and
  Cargo.lock but not the 92 `V1.0.0-RC.86` landing-page download stamps the rc.86 release commit
  had included, so the newest-tag comparison in CI's landing-constants guard went red for a day
  after the rc.87 tag. The sweep moved all 91 `V1.0.0-RC.86` stamps (plus the lowercase
  `data-latest-tag` span on the index) to RC.87 and regenerated the sitemap (55 lastmods moved).
  This is also the rule going forward: every rc roll includes the landing stamp sweep as its own
  commit, which this rc.88 roll follows (54 pages swept to RC.88, sitemap regen, 183 URLs).
- **The finalize-catalog read-back no longer trips the server's query-memory breaker** (part of issue
  [#1183](https://github.com/xerj-org/xerj/issues/1183), PR
  [#1207](https://github.com/xerj-org/xerj/pull/1207)) — the catalog read-back sent
  `size:0 + track_total_hits + min/max aggs` on each dataset's time field, and an agg with no columnar
  fast path makes the engine deep-clone every matching document into owned Values (~2 KB a doc) billed
  against `limits.max_query_memory_mb`. On the rc.86 xerj-search rebuild one ~570 k-record dataset
  estimated 1.1 GB against the 512 MB default, the server answered
  `429 circuit_breaking_exception: [request] query allocation (1.1gb) would exceed
  limits.max_query_memory_mb=512MB at [aggregation corpus materialisation]`, the client retried for its
  full 600 s envelope and honestly aborted (build b1791330708, 1,010,833 records indexed, existing
  corpus untouched) — retrying could never succeed because the request itself was the problem. The
  count now carries no aggs (a count needs no corpus) and the time bounds come from two `size:1`
  searches sorted asc/desc on the time field under the dataset's own filter — a sort reads doc values
  and clones nothing, and the first hit IS the extreme, so it is exact for the same reason the agg was.
  `epoch_ms_to_iso8601_utc` moved to `xerj_common::schema` so client and engine render numeric
  timestamps through one helper and catalog documents keep the agg's `value_as_string` shape. The
  finalize refresh — previously 1,479 serial per-index round trips that read as a stall for the better
  part of an hour against a node whose memory breaker was draining between them — is windowed: 50
  comma-joined index names per request, one progress tick per index. This is the read-back half
  only: [#1183](https://github.com/xerj-org/xerj/issues/1183) stays open until the standing
  xerj-search rebuild completes end-to-end against this client, which is the proof the fix was
  cut for.

## [1.0.0-rc.87] - 2026-10-07

### Performance

- **Phase-B bulk bodies are coalesced across files** (issue [#1147](https://github.com/xerj-org/xerj/issues/1147), PR [#1195](https://github.com/xerj-org/xerj/pull/1195), merged just past the rc.86 tag) — the per-file round-trip ceiling on corpus ingest, measured at ~10⁵ records/node-day on the ghsa-scale mirror class. Each file's records used to ship as their own `_bulk` body, so ingest throughput on a mirror of many single-record files was bounded by per-file round trips, not document volume. The shared bulk pipe now coalesces bodies across files: extraction and sending overlap, up to 8 bodies ride the wire at once (matching the server governor's `max_concurrent_bulks`), a worker about to park ships what is buffered once half the live workers are parked, the all-parked flush stays as the nothing-more-is-coming drain, and submit parks under backpressure when every wire slot is busy. Per-file ordering is preserved (a file's own cut chunks are sent synchronously by its worker). A/B'd on two corpus shapes with no regression — rustsec (1,965 files / 6,829 records): whole run 10.1 s → 9.7 s with phase B at parity; pysec (7,718 mostly-one-record files): 17.4 s → 16.9 s — and the structural win (one send per flush trigger, not one per file) shows at #1147's scale, which the standing corpus rebuild re-measures.

### Fixed

- **`xerj corpus index` now proves the store answers a search before it says "searchable"** (issue
  [#1183](https://github.com/xerj-org/xerj/issues/1183), PR
  [#1197](https://github.com/xerj-org/xerj/pull/1197)) — a resume over an index left with a
  dangling segment by a killed finalize printed `corpus 'xerj-search' searchable: 642 records`
  and exited 0 while the store answered every `_search` — and the salvage path's own
  `delete_by_query` — with `store_exception: Segment … not found`. `_count` is metadata-only and
  returned 642 over that same store, so every verification built on counts certified a corpus
  that could not be queried. Verification now has a third leg: one `size:0` `_search` over
  exactly the glob `xerj code` reads (`{prefix}-*`), with the node's own reason surfaced when it
  fails. In the build arm a build whose verification search fails is not verified — nothing is
  switched, the existing index stays what `xerj code` serves, and the unsearchable build's own
  indices are retired by exact name (an unsearchable index left under the namespace is what
  poisoned the live one); the #367 salvage can no longer record a store that cannot be searched.
  The update arm refuses to rewrite the ledger over a live corpus that cannot be searched, and
  the legacy arm — the exact live failure — refuses to print any verdict at all, because one
  unsearchable index anywhere under the wildcard makes "the corpus is queryable" untrue whatever
  the counts said. The probe is a `size:0` search, which the server already classifies as
  machine traffic in the audit log (#1109), so `xerj gain` still measures only the user's
  queries. The finalize-catalog deadlock that produces the dangling segment (item 1 of #1183)
  remains open, tracked in the same issue.
- **A request retrying after a transport error or a 5xx now says so on stderr** (part of issue
  [#1183](https://github.com/xerj-org/xerj/issues/1183), PR
  [#1199](https://github.com/xerj-org/xerj/pull/1199)) — the autoindex client's retry envelope
  is bounded (six attempts at a 300 s request timeout each, ≈31 min worst case) but was
  completely silent on everything except HTTP 429, which is how the #1183 incident read as a
  deadlock: a finalize-catalog request against a memory-pressured node spent eight minutes inside
  that envelope with zero output, and since `reqwest::blocking` parks the caller on a oneshot,
  every thread's `/proc` wchan read `futex_do_wait` — indistinguishable from a lock cycle at that
  granularity. Diagnosis in the issue: it was never a deadlock; the silence was the defect. A
  loading run (announce on) now prints `autoindex: search failed: …; retrying (attempt 2 of 6,
  … s in this request so far)` on the first failure and at most once per 30 s after, sharing the
  429 notice's clock so alternating failures cannot double-announce; probes, MCP clients and
  `--quiet` stay silent. The server-side stall root cause (the reference node running at its
  memory breaker during large rebuilds) remains open under #1183 and the #1122 memory-ceiling
  class.
- **A JSON record with no prose now carries a synthesized `text` passage, and a dataset that maps no text-searchable field says so at plan time** (the build-time half of issue [#1158](https://github.com/xerj-org/xerj/issues/1158), PR [#1200](https://github.com/xerj-org/xerj/pull/1200); the query-time half — falling back to an index's own text-typed fields — shipped in rc.86 as PR [#1185](https://github.com/xerj-org/xerj/pull/1185)). Raw-JSON corpora indexed as typed records (ids, enums, numbers, dates) were invisible to `xerj code` passage search: 36,263 GHSA advisories indexed to 193k records that answered `No passage matches` while the same index returned 10,000+ hits over its typed fields. Now a record whose flattened fields carry no prose-like string (≥ 24 chars and ≥ 4 tokens — below that a string is an id, an enum or a date, and maps `keyword`) gains a `text` passage rendered from its own fields (`key: value` lines, values capped at 256 chars, the passage at 4,096, scalar arrays joined so identifiers stay matchable); a record that already carries prose is left alone — the query side already searches it, and a rendered key/value soup would only outrank the real prose — and a record that owns a field named `text` keeps it untouched. Separately, any dataset whose sampled mapping contains no text-searchable field (a csv of enums, a keyword-only XML set) now warns at plan time: `dataset 'x' maps no text-searchable field — 'xerj code' passage search will find nothing in it (sampled fields: …)`, through the progress surface so `--quiet` stays silent. Scoped to the JSON family (whole-file and JSONL) — the measured class is raw-JSON mirrors; the same helper can ride the csv/yaml/xml data sites when a real corpus needs it. Existing state dirs re-provision their JSON datasets once to gain the field; every already-built corpus keeps working, and the 100 hub corpora are unaffected (their graded snapshots map standard fields).


## [1.0.0-rc.86] - 2026-10-06

### Security

- **Audit records now carry a server-derived `actor` class (`user`/`machine`)** (issue
  [#1109](https://github.com/xerj-org/xerj/issues/1109), PR
  [#1194](https://github.com/xerj-org/xerj/pull/1194)) — the audit log's
  search entries said WHO ran a query but not WHO IT SERVED, so the only way
  `xerj gain` could exclude autoindex's own machine traffic (#1105) was to
  re-derive it client-side from the note string (`size=0` probes) and the
  resource name (catalog sweeps): a heuristic a future note-format change
  would silently break, and one any caller writing audit-shaped entries could
  satisfy. Every audit entry now carries an `actor` field derived at append
  time by the SERVER from what the request itself is — a `size:0` counting
  probe or a search of the `autoindex-catalog` maintenance index is
  `machine`; everything else is `user`. Nothing a client can set (header,
  principal, body) is consulted — the spoofable `X-Xerj-Internal` header
  proposal from #1107 stays rejected. The class is part of the hash chain:
  flipping an entry from `user` to `machine` — the dodge that would remove
  unfavourable traffic from the `xerj gain` hit-rate denominator — breaks the
  chain like any other edit. Entries written by older binaries deserialize
  with `actor: "user"` (which is what they all were), so mixed-version logs
  still verify, and `xerj gain` prefers the field on new entries while
  keeping the note/resource derivation as the documented legacy fallback for
  old ones.

### Fixed

- **Raw-JSON corpora answered "No passage matches" for every query**
  (issue [#1158](https://github.com/xerj-org/xerj/issues/1158), PR
  [#1185](https://github.com/xerj-org/xerj/pull/1185)) — a raw-JSON
  corpus (advisory mirrors, OSV records) maps none of the standard
  content fields (`body`, `text`, …); the `xerj code` field resolver
  then fell to its "never an empty list" floor, `["body"]` — a field no
  index in the corpus maps, and on this engine an unmapped
  `multi_match` field collapses a multi-token query to zero hits with
  no error. The corpus built clean (193k docs) and was then invisible:
  0 hits under `xerj code` while the same indices returned 10,000+ hits
  queried directly (ghsa-db round-1 G7 0/5 was this). When no standard
  field is mapped, the floor is now the corpus's OWN text-typed
  properties (`ax_*` provenance excluded, sorted, capped at 24), and
  the passage renderer's last resort becomes the source's longest
  non-empty string field instead of an empty passage under a healthy
  header. Measured A/B on an advisory-shaped mapping: `fields=["body"]`
  → 0 hits (the defect); `fields=["details","summary"]` → 8 hits (the
  fix). Works retroactively on already-built corpora; the build-time
  half (extractor-side passage + a zero-text warning at index time)
  stays open on the issue.

- **`embedding.mode = "proxy"` with an unusable endpoint silently ran
  lexical** (issue [#1189](https://github.com/xerj-org/xerj/issues/1189), PR
  [#1193](https://github.com/xerj-org/xerj/pull/1193)) —
  an empty `default_endpoint` or a typo'd URL (`htp://…`) produced a
  startup WARN and a lexical embedder; indexing and semantic search then
  "succeeded" while writing lexical feature-hash vectors into the index, and
  `/v1/embedding/identity` reported `backend: lexical` for a config that
  said `proxy`. An explicit `mode = "proxy"` now fails closed at startup
  with an actionable error in both the embedder and the identity path
  (including the reporter's scheme typo, which carries the underlying
  `http or https` message); the auto path (endpoint set, mode not pinned)
  keeps the documented warn-plus-lexical fallback. `xerj autoindex` now
  names the live embedding backend in its run output so a degraded backend
  is visible where the work happens.

- **`embedding.batch_size` was never read** (issue
  [#1190](https://github.com/xerj-org/xerj/issues/1190), PR
  [#1193](https://github.com/xerj-org/xerj/pull/1193)) — the setting
  documented "Maximum documents per embedding API call (default: 64)" but
  no code read it: one `_bulk` of 20 documents left the proxy as a single
  request with 20 inputs, and an autoindex run was observed sending a
  192-input request. Providers cap the request `input` array (OpenAI 2048,
  self-hosted gateways often far lower), so a large bulk could fail on
  request shape, not load. The proxy now splits every batch into
  concurrent requests of at most `batch_size` inputs (each with its own
  retry/backoff and concurrency permit; the semaphore now limits in-flight
  HTTP requests, which was its purpose) and reassembles the vectors in
  input order. Verified by a mock-endpoint test: 10 texts with
  `batch_size: 4` leave as three requests of 4/4/2 inputs and the returned
  vectors keep input order across chunk boundaries.

- **Long BM25 queries silently fell back to per-segment IDF/avgdl**
  (issue [#1186](https://github.com/xerj-org/xerj/issues/1186), PR
  [#1188](https://github.com/xerj-org/xerj/pull/1188)) — the
  index-wide statistics pre-pass (#188) budgeted `(fields + terms) ×
  segments ≤ 4096`, so a many-term query on a multi-segment index crossed
  the cap, the pre-pass silently declined, and every segment scored
  against its own `df`/`avgdl`: padding a query with 1,500 terms that
  match NO document reordered the whole top-5 (reported `1-1:1.6797521`
  → `8-4:1.6904112`; after `_forcemerge?max_num_segments=1` both queries
  agreed — the merge had nothing to do with it, it just brought the
  probe count back under the cap). The budget is now split by cost
  class: field probes (one mmap'd FST per segment × field — the wide
  `query_string` explosion the cap existed for) stay at 4096, term
  probes (one FST seek per segment × term — the same order of cost
  execution itself pays for postings) get their own 1,048,576 budget, and
  every decline now logs a warning instead of changing the ranking
  silently. The reporter's literal repro (1,502 terms over a 32-core
  box's ~128 flush segments, ≈192k term probes) and a 4-shard/32-segment
  variant both now score identically to their unpadded forms,
  bit-for-bit.

- **`xerj code --mode hybrid` dropped lexical-only indices from the BM25
  leg** (issue [#1146](https://github.com/xerj-org/xerj/issues/1146), PR
  [#1177](https://github.com/xerj-org/xerj/pull/1177), Vincenzo Lombardo) — on
  a corpus where only some indices map `body` as `semantic_text`, the fused
  request went to the capable set alone (a semantic leg 400s on plain
  text), so every lexical-only index vanished from both legs while the note
  said "BM25 over N index(es)": 1 hit under hybrid where `--mode bm25`
  returned 20. The lexical-only indices now get the same native `hybrid`
  with its BM25 leg alone; the engine fuses per index, so both responses
  carry RRF scores on one scale and are merged by `_score` the way the
  engine merges indices — no client-side fusion. The note names them
  ("lexical-only (BM25 leg only, merged by score)"). Same repro, rc.83
  build: hybrid -k 20 → 20 hits (19 lexical-only + 1 semantic).

- **An index's analysis configuration could not be verified over the wire**
  (issue [#1092](https://github.com/xerj-org/xerj/issues/1092), PR
  [#1184](https://github.com/xerj-org/xerj/pull/1184), Vincenzo Lombardo) — search
  honoured a declared `analysis.analyzer.default`, but nothing an agent can
  read said so. `GET /{index}/_settings` and `GET /{index}` replayed an
  in-memory copy of the create body: after a restart it was empty, so the
  analysis block (and every other create-time setting) vanished, and a
  top-level `analysis` beside an `index` block was never echoed at all. Both
  now report `index.analysis` as the block the index's analyzer registry was
  actually built from — none for a pre-#204 index whose declared block is
  not honoured — and fall back to the persisted `settings.json` when the
  in-memory copy is gone. `POST /{index}/_analyze` was an inline lowercase
  splitter that ignored the index; it now runs the index's real pipelines:
  no `analyzer` → the index's default, `field` → the analyzer that field is
  indexed with, a named analyzer from the index's own registry. Ad-hoc
  `tokenizer` / `filter` / `char_filter` chains (names or inline
  definitions) are built by the same registry code index creation uses, with
  `keyword` as the tokenizer when only filters are given. Unknown analyzers,
  tokenizers and filters, `field` on the global endpoint and `normalizer`
  are 400 `illegal_argument_exception`. A missing index is 404. Checked
  side by side against OpenSearch 3.9.0: status codes, error types, tokens,
  offsets and positions match on all 24 probed requests. Only the wording of
  the reason differs on 3 of the 400s.

- **`sort: [{"_doc": ...}]` is arrival order, not `_id` order** (PR
  [#1127](https://github.com/xerj-org/xerj/pull/1127), Saurav Kumar) — the `_doc`
  sort key projected the document id string, so `_doc` pages ranked
  lexicographically by `_id` (matching neither ES index order nor XERJ's own
  stored order). The key is now the document's `seq_no`: numeric arrival
  order, unique per live doc (no ties), and a working `search_after` cursor.
  `_id` sort still echoes the id string.

### Community

- Two field reports, two fixes, and one first-time contributor landed in
  this window. Vinz2168 reported the hybrid BM25-leg defect on a real
  mixed-mapping corpus and verified the fix on it
  ([#1178](https://github.com/xerj-org/xerj/pull/1178)); the same reporter's
  `_settings`/`_analyze` work (#1092 above) went in through a maintainer
  rebase ([#1184](https://github.com/xerj-org/xerj/pull/1184)). Thomas
  Villani measured `xerj code` as a file-localization retriever over
  SWE-bench Lite and published the numbers
  ([#1187](https://github.com/xerj-org/xerj/pull/1187)). Saurav Kumar
  (Ravandevil25, whose `_doc` field report and CLA landed in rc.85) had
  his `_doc` arrival-order fix (#1127 above) merge after its CLA wait —
  his first code contribution to the project.
- Release-download metrics chore for 2026-10-06 (3,114 assets, 1,730
  binaries — direct commit, `[skip ci]`).

## [1.0.0-rc.85] - 2026-10-06

The cluster-ring-works window. One defect fix and one docs correction,
both born from the rc.83 three-node ring tests: the election churn that
made a ring with a dead peer elect a new leader every ~250 ms forever is
root-caused and fixed, and the clustering page now describes what the
ring actually does instead of an unshipped replication design.

### Fixed

- **A ring with a dead peer churned elections forever** (issue
  [#1168](https://github.com/xerj-org/xerj/issues/1168), PR
  [#1179](https://github.com/xerj-org/xerj/pull/1179)) — the documented
  convention lists the full membership in `[cluster] peers` on every
  node, self included, and nothing removed this node's own entry before
  the membership reached Raft. Two compounding consequences: the peer
  count included self, so a three-node ring computed a *four*-member
  cluster whose majority was three — unanimity, zero fault tolerance —
  and self was also a routable transport peer, so each candidate sent
  its own `RequestVote` to itself over loopback TCP where a re-grant
  counted the self vote **twice**. After the leader died, no survivor
  could assemble three real votes, and the double-counted self votes
  let the surviving pair hand each other phantom wins while leapfrogging
  terms indefinitely. Self is now excluded at three layers (config
  parse in `main.rs`, `RaftNode::new`, and `TcpTransport::new`), each
  logging a warning when it fires. Measured on a three-node loopback
  ring (`tick_ms=50`, kill the leader, 240 s observation): rc.83
  churned through **94 terms** and never stopped; with the transport
  starvation fix alone (per-peer outbound queues, first commit on the
  PR) the churn got *faster* — **909 terms**, which is what pinned the
  real root cause; with self-exclusion, a new leader is elected
  **~250 ms** after the kill (one split-vote retry) and the ring then
  holds **zero elections for the remaining 239 s**, with the second
  survivor never even timing out. Three new tests, including the ring
  acceptance scenario `ring_with_full_membership_lists_survives_leader_death`
  driving three `ClusterNode`s constructed exactly as the server passes
  the full-membership config.

### Documentation

- **The clustering page described an unshipped replication design**
  (PR
  [#1180](https://github.com/xerj-org/xerj/pull/1180), issue
  [#1170](https://github.com/xerj-org/xerj/issues/1170) context) —
  `docs/clustering` claimed metadata replication, document replication,
  node drain/activate/remove endpoints, region split/merge, and a
  `NodeState` API, none of which exist in the code; it even invented
  `/_cluster/health` output. The page now states what the ring does
  today — leader election (~250 ms failover, citing the #1168 fix), an
  HMAC-authenticated cluster transport with dead-peer isolation, and a
  durable local Raft log — and says plainly that metadata and document
  replication are roadmap (#1170), with a "when not to cluster: today,
  almost always" section. The same sweep corrected the docs-index
  cards across 29 pages (145 replacements), the troubleshooting
  election-flapping entry (sustained churn is a *defect* now, not a
  tuning knob), the rolling-upgrade page (the cluster wire has no
  version negotiation — full stop/start, not rolling), and the
  clustered backup section (indexes are node-local today).

## [1.0.0-rc.84] - 2026-10-06

The tell-the-truth window. Every signal an operator reads — cluster
health, node counts, corpus-index exit codes, the landing counts — now
reports what actually happened instead of what was configured or hoped
for. A small engine window on top of rc.83's corpus-hub hundred: two
cluster-honesty PRs, one corpus-tooling honesty PR, and two docs PRs
(#1167, and #1163 by
[thomas-villani](https://github.com/thomas-villani), his fourth merged
PR).

### Fixed


- **Cluster endpoints fabricated topology** (issue
  [#1169](https://github.com/xerj-org/xerj/issues/1169), PR
  [#1172](https://github.com/xerj-org/xerj/pull/1172)) —
  `GET /_cluster/health?wait_for_nodes=N` echoed `N` back as
  `number_of_nodes` with `status: green` and `timed_out: false`, so the
  multinode smoke suite "converged" on a single node that had no cluster
  at all (the handler comment said so out loud). Health now reports the
  real configured membership (`1` when cluster mode is off, else the
  distinct own + peer addresses), an unsatisfiable `wait_for_nodes` times
  out honestly (408, `timed_out: true`, count stays real), and upper
  bounds (`<=N`, `<N`, `le(N)`) are enforced instead of collapsed to
  "always satisfied". `_cat/nodes` lists every configured member (peers
  with `-` placeholders, no invented metrics), `_nodes` totals the
  membership while `successful: 1` reflects that only self answered, and
  `_cluster/state` reports this node's actual cluster address instead of
  the never-bound 9300 default and `master_node: null` on a configured
  ring rather than every node claiming mastership. The multinode smoke
  case now **skips with a recorded reason** instead of passing vacuously;
  `cluster.health/20_request_timeout` passes through the real timeout
  path. Cluster config that could only fail silently is refused at load
  (issue [#1171](https://github.com/xerj-org/xerj/issues/1171)): a peer
  label differing from its address (which made the node lose every
  election with no diagnostic of its own) and `cluster.port` colliding
  with this node's own rest/grpc/es_compat port (which degraded the node
  to single-node with green health).
- **A degraded cluster boot was invisible after the first log line**
  (issue [#1171](https://github.com/xerj-org/xerj/issues/1171), PR
  [#1175](https://github.com/xerj-org/xerj/pull/1175)) — a configured
  ring whose TCP transport failed to bind, or whose cluster-state storage
  was unavailable, degraded to single-node with one ERROR line at boot
  and then `status: green` forever on `/_cluster/health`: the operator
  believed the node was clustered and no API could say otherwise. The
  boot outcome is now recorded on the application state
  (`ClusterTransportStatus`: Disabled / Running / Degraded) before the
  first router exists, and health answers from it: `status: yellow`,
  `number_of_nodes: 1` (the node actually serving — counting the
  unreachable configured peers would be the same fabrication #1169
  removed), and an additive `xerj_cluster_transport: "degraded"` marker
  that says *why* it is yellow, present exactly when true. Waits stay
  honest from the degraded side too (`wait_for_nodes=2` of the one
  serving node → 408 with the real count), and red still outranks the
  degraded-yellow (an unopenable index remains the headline, with the
  transport marker beside it).
- **`xerj corpus index` reported a converged state as a failure and an
  uncountable node as "no new records"** (issue
  [#1173](https://github.com/xerj-org/xerj/issues/1173), honesty half —
  the crawl-throughput half stays open, PR
  [#1174](https://github.com/xerj-org/xerj/pull/1174)) — the resume
  cleanup's `_delete_by_query` against an index a namespace swap had
  already retired answered 404 and killed the run (a missing index cannot
  hold the stale documents the cleanup exists to remove); 404 is now the
  answer "deleted nothing", the same rule `_count` and `_cat/indices`
  already follow. And when a failed run's before/after counts could not
  be read from the node, the tool asserted "wrote no new records" — that
  claim now requires both counts to be known; an unreadable count prints
  UNKNOWN.
- **The hub counts went stale again** (PR
  [#1167](https://github.com/xerj-org/xerj/pull/1167)) — `llms.txt:51`
  and the corpus-hub blog's dated update note still said `75 live / 104
  backlog rows` from 2026-10-02 while the registry had reached 100 live /
  119 backlog on 2026-10-05. Both surfaces now say 100/119 and name the
  rc.83 skill-table milestone — the same line, the same drift class, as
  the llms.txt:50 pack-status fix queued after rc.82.
- **EPUB and notebook answers that lagged the extractors** (PR
  [#1163](https://github.com/xerj-org/xerj/pull/1163) by
  [thomas-villani](https://github.com/thomas-villani)) — the notebook
  search and EPUB answer pages were rewritten from fresh captures on the
  rc.83 build (the first published binary with both extractors), moving
  the fact-check matrix rows for notebooks and EPUB to GREEN; the
  Parquet row widened to Parquet/Arrow/HDF5/NumPy/pickle (named with an
  export hint, still no reader). The man-page fact-check aliases had held
  a literal `\x08` since a scripted edit in #1130's docs commit decoded
  `\b` into the control character — they now use real word boundaries,
  and `factcheck.py --self-test` fails if any alias or rule pattern holds
  a control character, so the class cannot silently return.

## [1.0.0-rc.83] - 2026-10-06

The corpus-hub hundred release. The engine window is small — one retrieval
fix and three extractor PRs — while the reference-coding catalogue reached
**100 live, licence-reviewed, G7-graded corpora at
[hub.xerj.org](https://hub.xerj.org)**, every one of them now named in the
`xerj-code` skill's domain-selection table (PR
[#1164](https://github.com/xerj-org/xerj/pull/1164); the five SKILL-row
additions of the rc.82 window rode in as PR
[#1157](https://github.com/xerj-org/xerj/pull/1157)).

### Fixed

- **The version roll that missed itself** (PR
  [#1155](https://github.com/xerj-org/xerj/pull/1155)) — the rc.82 release
  PR bumped only the download link's `data-latest-tag`; the 91 footer brand
  spans across the landing pages still read `RC.81`, and the release's own
  CI could not see the stale ones because the newest tag at push time was
  still rc.81. All spans now roll with the release and the guard compares
  against the release being cut, not the last one tagged. `xxhash` is
  bumped to 0.8.19 in the same PR (the dependabot PR it supersedes was
  #1152, closed).
- **The vector leg of `xerj code` hybrid/semantic retrieval** (issues
  [#1145](https://github.com/xerj-org/xerj/issues/1145) and
  [#1148](https://github.com/xerj-org/xerj/issues/1148), PR
  [#1153](https://github.com/xerj-org/xerj/pull/1153) by
  [thomas-villani](https://github.com/thomas-villani)) — the semantic
  clause went out with no `k`, so the parser default (10) capped the
  vector pool whatever the request size said: `--mode semantic -k 20`
  returned 10 hits, and hybrid fused BM25@50 with vector@10. The clause
  now carries the fetch window as `k`. And `--lang` sat beside the
  semantic clause in `bool.must`, a shape the engine's vector dispatch
  does not peel — the request silently fell through to the lexical path
  and answered 200 with zero hits. The language constraint now rides in
  `bool.filter`, on both hybrid legs, keeping the semantic clause the
  bool's single `must` — the exact shape the dispatcher peels.
- **Corpus generation hygiene** (issue
  [#1136](https://github.com/xerj-org/xerj/issues/1136)) — stale index
  generations no longer survive beside a corpus, silently poisoning its
  namespace. Three changes in `xerj corpus index` / `xerj code`:
  a legacy-mode run (a state file that names no build) now retires the
  stamped generations it supersedes — the measured shape was
  `tldr-pages` carrying an unstamped 38,554-document generation beside
  two stamped ones, mixed into every `xerj code` answer; a build whose
  namespace listing the node never answered says so loudly and retires
  nothing instead of reading the empty list as "nothing to retire"; and
  every verified `--fresh` swap ends with an epilogue naming whatever
  still sits under `xc-<corpus>-` outside the build readers are pinned
  to. `xerj code` now also prints a stderr notice when a corpus has no
  verified build recorded and the query is widening to the whole
  namespace.
- **HTML tables and paragraph boundaries in autoindex** (PR
  [#1154](https://github.com/xerj-org/xerj/pull/1154) by
  [thomas-villani](https://github.com/thomas-villani)) — a table on a
  page indexed as one document lost its text twice over: it was not the
  dominant table (so not emitted as rows) and cell text never reached
  the body. Small tables — option lists, version matrices, the common
  case on documentation pages — were silently unindexed. The rows now
  go to the body in place, one ` | `-joined line per row, the shape
  PPTX tables already use. And `<p>`/`<div>`/list ends now end a
  paragraph, so `split_sections` breaks pages at paragraph ends instead
  of hard-cutting every 2 KB mid-word behind a heading emitted as its
  own empty section.
- **EPUB books in autoindex** (PR
  [#1159](https://github.com/xerj-org/xerj/pull/1159) by
  [thomas-villani](https://github.com/thomas-villani)) — one record per
  spine document in reading order, cut at the table-of-contents anchors,
  with the book's `title`/`author`/`language` and each chapter's own
  title from the nav or NCX. DRM-encrypted parts are detected and
  skipped, and per-part and total byte budgets apply, so a hostile
  container cannot balloon the index.
- **A section the next one repeats whole is not emitted** (PR
  [#1160](https://github.com/xerj-org/xerj/pull/1160) by
  [thomas-villani](https://github.com/thomas-villani)) — a short
  paragraph (a chapter heading, seen on a Gutenberg EPUB as "CHAPTER 26.
  Knights and Squires." alone) followed by one too big to join it was
  emitted as a section of its own, and the next section then carried it
  whole as its overlap: a record with no text of its own. A section
  shorter than the overlap now stays and leads its successor instead.
- **Jupyter notebooks, and data files that cannot be read, in
  autoindex** (PR [#1161](https://github.com/xerj-org/xerj/pull/1161)
  by [thomas-villani](https://github.com/thomas-villani), landed via the
  maintainer rebase
  [#1162](https://github.com/xerj-org/xerj/pull/1162)) — a notebook is
  grouped the way a reader sees it: a section starts at each Markdown
  heading and holds every cell to the next one, code fenced in the
  kernel's language (R and Julia notebooks say so), text outputs capped
  and ANSI-stripped, images skipped, nothing ever executed; the
  `cell` field gives the 1-based index an agent needs to find the
  section in Jupyter. Parquet, Arrow, NumPy, HDF5 and pickle files —
  binary shapes no extractor should parse — are now named records that
  say which tool exports them to an indexable form.

## [1.0.0-rc.82] - 2026-10-05

The documents-people-actually-have release. Three extractor PRs from a
first-time contributor extend autoindex to the formats that were still
read-as-text: Unix man pages, and the two spreadsheet shapes real
workbooks ship — vertically merged cells and two-row grouped headers.
Every code change in the window came through the community surface:
the extractor work is
[thomas-villani](https://github.com/thomas-villani)'s first contributions
to the project, and the window also carries the `xerj code` retrieval
fixes and two field reports.

### Added

- **Man page extraction** — roff `man(7)` sources read as documents, one
  record per `.SH` section: the `.TH` title (`LS(1)`), the NAME summary,
  font escapes dropped, `\(em`-class specials become the characters they
  name, no-fill blocks keep their layout, and a tbl table becomes one
  ` | `-joined line per row. Enough of the roff request language is read
  (`.ds` strings, `.if`/`.ie`/`.el` conditionals, `.de` macro bodies) that
  pod2man's preamble needs no special case; mdoc(7) pages and `.so` stubs
  stay text, as before. Ported from the man parser in all2md
  (`parsers/man.py`, MIT), reduced to plain text with attribution
  (PR [#1134](https://github.com/xerj-org/xerj/pull/1134),
  thomas-villani).
- **XLSX merged cells fill down** — a cell merged DOWN over several rows
  (a pandas MultiIndex export, a category label beside its line items)
  stores its value only in the top row; every covered row now gets it, so
  `region: East` matches all of East's rows and a `terms` aggregation
  counts them. Merges across columns are deliberately not expanded
  (PR [#1132](https://github.com/xerj-org/xerj/pull/1132),
  thomas-villani).
- **XLSX two-row grouped headers** — a header two rows deep (group labels
  above the column names, pandas' MultiIndex columns) names each column
  from both rows: `Q1_Jan`, `Q1_Feb`. Three-or-more-row headers keep
  their two lowest rows; on sheets over 64 MB the sample keeps one
  (PR [#1133](https://github.com/xerj-org/xerj/pull/1133),
  thomas-villani, stacked on #1132).
- **Docs: extracted formats** — `.pptx`/`.xlsx` listed as extracted
  formats across the docs and the retired "no XLSX/PPTX extractor"
  caveats removed
  (PR [#1130](https://github.com/xerj-org/xerj/pull/1130),
  thomas-villani).
- **The xerj-code skill points at the whole hub** — per-project corpus
  selection now walks the live registry instead of a hand-listed few
  (PR [#1126](https://github.com/xerj-org/xerj/pull/1126)), and the
  domain-selection table covers every live hub corpus
  (PRs [#1143](https://github.com/xerj-org/xerj/pull/1143),
  [#1149](https://github.com/xerj-org/xerj/pull/1149),
  [#1150](https://github.com/xerj-org/xerj/pull/1150)).

### Fixed

- **`xerj code` top-k crowding** — one large file's adjacent 2 KB chunks
  no longer crowd every other source out of the top-k: at most 2 records
  per source file (issue [#1137](https://github.com/xerj-org/xerj/issues/1137),
  fixed by PR [#1140](https://github.com/xerj-org/xerj/pull/1140) — the
  failure class was measured as G7 retrieval fails before the fix).
- **`xerj code` txt-family search** — the txt extraction family puts its
  content in a `text` field the BM25 leg never searched, so passages
  rendered empty; the field is searched now (issue
  [#1139](https://github.com/xerj-org/xerj/issues/1139), fixed by PR
  [#1140](https://github.com/xerj-org/xerj/pull/1140)).
- **Markdown sniffing** — markdown whose body starts with YAML-shaped
  keys or an HTML prefix classified as Yaml or Xml and indexed as junk;
  it is Txt now, with guards on the sniffs that mistook it
  (PRs [#1142](https://github.com/xerj-org/xerj/pull/1142) and
  [#1144](https://github.com/xerj-org/xerj/pull/1144)).
- **`.odp` advice** — the unsupported-format hint now tells OpenDocument
  presentation owners to save as `.pptx` (one record per slide, with
  slide titles and notes) rather than export to PDF
  (PR [#1131](https://github.com/xerj-org/xerj/pull/1131),
  thomas-villani).

### Community

- Field reports: a second reference-coding report from
  [alessandropcostabr](https://github.com/alessandropcostabr)
  (PR [#1141](https://github.com/xerj-org/xerj/pull/1141)) and a
  doc-sort field report from
  [Ravandevil25](https://github.com/Ravandevil25)
  (PR [#1128](https://github.com/xerj-org/xerj/pull/1128)), whose CLA
  signature landed with it (PR
  [#1129](https://github.com/xerj-org/xerj/pull/1129)) — the `_doc`
  arrival-order fix that report describes is reviewed and waiting on its
  own CLA signature (PR
  [#1127](https://github.com/xerj-org/xerj/pull/1127)).
- Release-download metrics chore for 2026-10-04 (2,956 assets, 1,651
  binaries — direct commit, `[skip ci]`).

## [1.0.0-rc.81] - 2026-10-03

The agent-intake release — everything between rc.80 and this cut serves the
same turn: an agent (or a first-time user) points XERJ at a folder or a
document and gets a searchable index without being told "unzip it first" or
silently indexing junk. The window's spine is the **zero-experience E2E
harness** ([#1106](https://github.com/xerj-org/xerj/pull/1106)):
`demo/playbooks/AGENT_FTX_HARNESS.md` runs a no-prior-knowledge agent
through folder onboarding end to end with a reference run recorded — and its
findings were fixed as defects, not filed and forgotten: the naive path's
four E2E failures ([#1107](https://github.com/xerj-org/xerj/pull/1107),
issues #1102–#1105), JSONL with a fat first line indexing as one junk
record ([#1113](https://github.com/xerj-org/xerj/pull/1113)), and Office /
OpenDocument / EPUB archives named instead of advised-to-unzip
([#1114](https://github.com/xerj-org/xerj/pull/1114)). Two new extractors
join the autoindex family on the same one-record-per-unit rule:
PowerPoint decks, one record per slide
([#1117](https://github.com/xerj-org/xerj/pull/1117)), and Excel workbooks,
one dataset per sheet ([#1124](https://github.com/xerj-org/xerj/pull/1124),
carrying the community contribution of #1120 rebased). The engine fix the
window is proudest of came from our own corpus-indexing crash: under memory
exhaustion a background `std::thread::spawn` **panics** (never returns
`Err`), and with `panic = "abort"` one ENOMEM at flush time took the whole
node down (issue [#1122](https://github.com/xerj-org/xerj/issues/1122)) —
[#1123](https://github.com/xerj-org/xerj/pull/1123) adds a governor-backed
`thread_spawn_safe` gate so the drain-free path drops inline instead of
aborting the process. The **Corpus Hub** shipped publicly at
[hub.xerj.org](https://hub.xerj.org) — registry-driven, every live corpus
G7-graded against pre-registered suites
([#1119](https://github.com/xerj-org/xerj/pull/1119)) — and the corpus
A/B benchmark behind its honesty rule landed with the tie published
([#1116](https://github.com/xerj-org/xerj/pull/1116), issue #1111: no X
finding traces to corpus content; 33 confirmed defect classes, 2 shared).
Strict unknown-field refusal ([#1112](https://github.com/xerj-org/xerj/pull/1112))
closed defect #1093: opt-in strict mode 4xx's unknown lookup fields, and
autoindex-created indices are strict at create. CLA signature
[#1115](https://github.com/xerj-org/xerj/pull/1115) is process, not product.

### Added

- **Zero-experience E2E harness** — `AGENT_FTX_HARNESS.md` playbook with the
  rc.80 reference run; re-runnable on shell, MCP-only and HTTP-only personas
  (PR [#1106](https://github.com/xerj-org/xerj/pull/1106)).
- **PowerPoint extraction** — `.pptx` autoindexed as one record per slide
  (title, notes, shape text), joining DOCX/PDF/EPUB in the document family
  (PR [#1117](https://github.com/xerj-org/xerj/pull/1117)).
- **Excel extraction** — `.xlsx` autoindexed as one dataset per sheet with
  headered columns typed from the first data row (PR
  [#1124](https://github.com/xerj-org/xerj/pull/1124); community contribution
  #1120 rebased onto main).
- **Corpus Hub** — public registry site at hub.xerj.org, generated purely
  from the corpus-hub branch registry; live status requires a G7 grade
  against a suite pre-registered before retrieval (PR
  [#1119](https://github.com/xerj-org/xerj/pull/1119)).

### Fixed

- **#1122 — node abort under memory pressure.** `std::thread::Builder::spawn`
  panics (not `Err`) when the alternate signal stack cannot be mmap'd;
  `panic = "abort"` turned that into SIGABRT for the whole process at flush
  time under RSS exhaustion. `ResourceGovernor::thread_spawn_safe` now gates
  the drain-free spawn: breaker engaged or live RSS ≥ watermark → drop
  inline (PR [#1123](https://github.com/xerj-org/xerj/pull/1123); root-caused
  from the persistent server log, repro and three fix directions on the
  issue).
- **Zero-experience naive path** — the four E2E defects the FTX harness
  filed: wrong index-name derivation, progress-stream shapes a first-time
  agent cannot parse, a misleading success on empty folders, and the
  shell-assumption in the onboarding prose (PR
  [#1107](https://github.com/xerj-org/xerj/pull/1107), issues
  [#1102](https://github.com/xerj-org/xerj/issues/1102)–[#1105](https://github.com/xerj-org/xerj/issues/1105)).
- **JSONL fat-first-line** — a first line larger than the sniff window sent
  the whole file to one junk record; JSONL now indexes per line (PR
  [#1113](https://github.com/xerj-org/xerj/pull/1113)).
- **Office/OpenDocument/EPUB zips named** — autoindex extracts them itself;
  the "run unzip" advice is gone (PR
  [#1114](https://github.com/xerj-org/xerj/pull/1114)).
- **#1093 — unknown lookup fields silently 0-hit.** Opt-in strict mode
  returns a 4xx for unknown fields in `term`/`match`/`range`/`exists`;
  autoindex indices are strict at create (PR
  [#1112](https://github.com/xerj-org/xerj/pull/1112)).

### Benchmarks

- **Corpus A/B (#1111)** — fair two-arm protocol over 71 corpora; the tie
  published: no X-arm finding traces to corpus content, P 13 vs X 16
  dynamic-exploitable tasks, 33 confirmed defect classes of which 2 shared
  (PR [#1116](https://github.com/xerj-org/xerj/pull/1116)).

## [1.0.0-rc.80] - 2026-09-30

The measured-gates release — the ten-item program to the first official
release ([discussion #1054](https://github.com/xerj-org/xerj/discussions/1054),
issues #1055–#1064) executed the way it was filed: every item landed with its
acceptance criterion measured on the release binary, and **the failures ship in
these notes as failures**. Three gates passed outright (`/_ask` 0.9975 macro-F1
at 1.129 ms p50; `xerj_map` unknown-field 400s 100% reduction; calibration
ECE 0.3109 → 0.0088 held-out), three failed and stay opt-in or behind a flag
with the numbers published (the lexical judge, the flywheel cache share, the
tier-2 decide head's SMS/AG News bars), one split (stemming: +0.0160/+0.0179
BM25 nDCG@10, zero-hits 25 → 15 against ≤ 10). The decide ladder is the
release's spine — tier-2 local head ([#1072](https://github.com/xerj-org/xerj/pull/1072)),
CLI arming + default-feature fix ([#1073](https://github.com/xerj-org/xerj/pull/1073),
[#1097](https://github.com/xerj-org/xerj/pull/1097)), calibration `p_cal` beside
every `p_raw` ([#1080](https://github.com/xerj-org/xerj/pull/1080),
[#1087](https://github.com/xerj-org/xerj/pull/1087)), the answer flywheel
([#1075](https://github.com/xerj-org/xerj/pull/1075)) and `_watcher` evaluating
one real watch shape ([#1082](https://github.com/xerj-org/xerj/pull/1082)) — and
the agent surface grew the twelfth and thirteenth MCP tools (`xerj_map`,
`xerj_plan` behind the new `POST /_ask`) plus token-budgeted MCP responses.
The console closes the user gate the UX reviews kept failing: a knowledge
surface that shows what is indexed using only the engine's own numbers
(PR [#1095](https://github.com/xerj-org/xerj/pull/1095), carrying the
#1098/#1099 honest-claims fixes from its own screenshot review). rc.79 was folded into
this cut by direction — no rc.79 tag exists.

### Added

- **Console: one knowledge surface — size, structure, relations and
  capabilities after indexing (PR
  [#1095](https://github.com/xerj-org/xerj/pull/1095)).** The moment
  `xerj brain` finishes, the Console's landing view (CORPUS) now answers
  the three questions a person has, in one session-authorized read —
  `GET /_xerj-console/api/v1/knowledge` (`xerj-console-api/src/knowledge.rs`):
  whole-corpus totals (datasets/records/files/source bytes plus the
  engine's live doc count), every catalog dataset with its FULL field list
  (type, semantic flag, cardinality, null%, coverage, example values — the
  same `FieldSpec` data `xerj autoindex map` serves agents) joined with
  live doc counts and on-disk store bytes, the cross-dataset relations
  autoindex actually inferred (key overlaps, time alignments — none
  inferred is shown as none, never guessed), other user indices, and a
  capability strip grounded in real surfaces (console routes, CLI
  commands, `POST /_ask`, `POST /_decide`; `graph` only when a readable
  brain exists, `ask`/`map` only when a catalog exists). Simplifications
  in the same PR: the fabricated clusters/index/field mocks behind the
  DATA section are deleted (real-only, failures yield empty lists), its
  field table drops type-guessed encoding/ratio columns, Second Brain is
  reworded into the graph layer of the corpus rather than a notes-flavored
  mode, and `xerj mcp` help now says 13 tools (it said 11). Pinned on
  payloads by `xerj-console-api/tests/knowledge_surface.rs` (5) and
  `xerj-ux/test/knowledge-surface.test.mjs` (7). Two honest-claims fixes
  from that PR's screenshot review, riding the same branch (issues
  [#1098](https://github.com/xerj-org/xerj/issues/1098) and
  [#1099](https://github.com/xerj-org/xerj/issues/1099)): the CORPUS fields
  table now leads with the corpus's DOMINANT fields, ranked by the
  catalog's own coverage × avg_len with autoindex plumbing last (a
  type-first rank made a 96%-code corpus open with 0.6%-coverage email
  fields and pushed its `code` field out of the table), unmeasured
  coverage renders as "—" on both card paths (never a defaulted 100%), and
  the whole table is one ranked list through the ALL-FIELDS disclosure; and
  the capability strip gained a server-computed `semantic` entry carrying
  the node's own facts — field, companion vector field, dims, similarity
  and the embedder's own label ("lexical feature-hash (built-in, 384-dim,
  non-neural)" on a default node, with the `--embed-mode neural` pointer)
  — with NO vector count, because the engine exposes none on any stats
  surface; an index whose elected semantic field carries no embedding says
  so in as many words.
- **rc.80 decide-gate measurements: tier-2 quality, serving latency, and the
  flywheel replay, all through live nodes
  (PR [#1088](https://github.com/xerj-org/xerj/pull/1088)).** The rc.79/rc.80
  gates on the System One decide ladder, measured on commit `a179e1ef3`
  (`xerj v1.0.0-rc.78`) in `benchmarks/decisions-as-retrieval/results/2026-09-30-rc80gates/`.
  #1057 quality **FAIL** (bars: SMS acc ≥0.95 / ECE ≤0.05, AG News ≥0.85):
  SMS 0.9193 / ECE 0.287 on the 1,574-row seed-7 held-out split; AG News
  0.2599 on the full 7,600-row test set (chance 0.25 — the artifact was never
  trained on it); Banking77 reported, not gated: 0.1185 / ECE 0.0717, which
  reproduces the eval card exactly through the node's choice path. #1057
  latency **PASS**: p50 8.4 ms for 30 `/_decide` noul questions with the node
  pinned to 8 cores (5-way choice 20.0 ms; the full 77-way vocabulary 282.6
  ms); the tier downloads 0 MB — there is no download path — and the artifact
  is 28.5 MiB of F32 (not quantized; candle 0.9 has no quantized safetensors
  loader). #1061 flywheel replay **FAIL**: as shipped, the write-back caches
  only 12 answers in 2,000 requests (once any cached doc gives BM25 support
  the history vote answers everything, and history answers are never cached),
  then answers 99.7% of later traffic from that frozen cache at 0.033
  accuracy — 4.91% of later traffic at ≥0.8 confidence (bar 80%) at 0.0377
  accuracy (bar 0.97); with the gate's premise instantiated (2,000 gold
  answers cached), 19.72% of later traffic clears 0.8 confidence at 0.9953
  accuracy — the accuracy bar holds, the 80% share bar fails 4× under at
  k=10. Release-cut fact recorded beside the numbers: **the stock release
  binary is built without the `decide-local` cargo feature** —
  `--decide-mode local` on it logs a warning and tier 2 stays off, so the
  tier ships absent, not merely flagged off, until release.yml enables the
  feature.

- **`xerj-decide-v1`: the open tier-2 decide model — training/export harness,
  measured eval card, upload-ready bundle
  ([#1064](https://github.com/xerj-org/xerj/issues/1064), PR
  [#1084](https://github.com/xerj-org/xerj/pull/1084)).** The decide
  ladder's tier 2 (`--decide-mode local --decide-model-dir <dir>`) until now
  had a loader and no artifact to load. `benchmarks/decide-model/` is the
  deterministic harness that trains one: stdlib-plus-candle (CPU only), the
  same `candle_transformers` ModernBERT `load` call the server loads with —
  trainer and server run one forward, not two that might drift — splitmix64
  for every random choice including weight init (candle's CPU RNG cannot be
  seeded), per-epoch `stream_seed`, no clock or environment read in the
  artifact path, and the one process-unstable step (the tokenizers crate's
  wordpiece *fit*; measured 8,740/8,741/8,742 across runs) shipped as a
  frozen committed input, `pinned/tokenizer.json`. The artifact — 7,419,907
  params (256×6×4, FFN 768, vocab 8,742), 29,683,844 bytes of F32
  safetensors — is verified by loading it back through `--decide-model-dir`
  on a booted node; the weights exceed the 5 MB blob ceiling so the repo
  carries the export script, manifest, config, tokenizer and VERSION, and
  `scripts/publish_bundle.sh` assembles the complete upload bundle
  (Apache-2.0 licence, digest-checked manifest, the card as README) with the
  exact `huggingface-cli`/`curl` commands — publication is PENDING operator
  credentials, and no published URL is claimed. Every number is measured and
  in `docs/DECIDE_MODEL.md`: full-test-split Banking77 77-way choice
  0.1185 acc / 0.072 ECE; SMS held-out noul 0.8850 acc / 0.310 ECE / F1
  0.686; leave-one-out zero-shot ≈ chance in both directions (Banking77
  0.0094 vs 1/77 ≈ 0.013 chance; SMS 0.1982) — the honest boundary of the
  "no history" claim; serving latency on a booted private-port node (release
  build, CPU, sequential client): `/_decide` noul p50 13.0 ms / p99 13.9,
  `/v1/systemone` noul 13.2/14.3, 5-way choice 21.6/23.5, full 77-way
  228.1/237.1; against the history vote's published 0.819 BM25 / 0.937
  hybrid (Banking77) and 0.983 BM25 (SMS), tier 1 stays ahead on its home
  ground, as it should. Wide-choice ranking and calibration at this size are
  named as unsolved in the card, not buried. Reference-coding corpora are
  absent in this sandbox and were not consulted.
- **benchmarks: `xerj_map` unknown-field-400 gate measured — 20/30 → 0/30
  (PASS) ([#1055](https://github.com/xerj-org/xerj/issues/1055), PR
  [#1086](https://github.com/xerj-org/xerj/pull/1086)).** The rc.79 gate for
  `xerj_map` ("unknown-field 400s from MCP-issued DSL down ≥ 80 % on the
  reference-coding harness") is now a measured number with raw results
  (`benchmarks/map-gate/`), per the plan's packaging rule. On the release
  binary against a reference-coding corpus (the three small pinned
  `xerj-vector` repos, 9 datasets, 3472 records), 30 MCP-shaped query intents
  issued twice — once with field names guessed the way an ES-fluent agent
  writes them first, once with field choices derived mechanically from the
  real `xerj mcp` → `xerj_map` stdio response: **20 of 30 guessed-DSL
  requests failed with an unknown-field HTTP 400 (9 sort, 5 semantic,
  6 knn/hybrid-leg), 0 of 30 map-informed requests did — a 100 % reduction
  against the ≥ 80 % threshold.** Discovery recorded beside it: on this
  engine a wrong field in `term`/`match`/`range`/`exists` is a *silent*
  0-hit 200, not a 400; the 400 class is exactly `sort` without
  `unmapped_type`, `knn` on a non-vector field, and `semantic` on a
  non-`semantic_text` field. The silent-miss class is reported separately
  (5 vs 1), not folded into the gate number. The live model-driven agent
  loop (whether an agent consults the map unprompted) is designed but not
  run — live-model cost not approved for this wave; design and ~$120
  estimate in the benchmark README.
- **systemone: calibration layer — `p_cal` beside every `p_raw`, published
  reliability curve ([#1063](https://github.com/xerj-org/xerj/issues/1063), PR
  [#1080](https://github.com/xerj-org/xerj/pull/1080)).** A ladder
  probability ranks before it odds — on the FiQA rerank baseline a hosted
  noul of 0.93 meant relevance 34% of the time (ECE 0.3109). The new
  `[decisions] calibration = isotonic | temperature` setting (default `none`,
  the section's 8th setting) fits a correction on a deterministic held-out
  fifth of the node's recorded outcomes — outcome documents carry the truth
  label, the `p` that was served for that label, and NO `source` field
  (flywheel answers are predictions; a fit on predictions would learn the
  model is right by construction), 10 labelled pairs minimum — and every
  probability-shipping path on the decide surface carries the calibrated
  probability BESIDE the raw one: `/_decide` answers `p_raw` (the positive
  label's share, the quantity a noul answers) plus `p_cal`;
  `/v1/systemone` carries both in `decisions.evidence` per noul question
  with the strictly-parsed answer objects untouched; choice per-option
  shares are a different quantity and are not calibrated (the block says
  `"scope": "noul"`). No calibration configured → no `p_cal` field at all;
  configured but unfitted → `p_cal: null` with a published, actionable
  reason; fitted → the fit's value, never a copy of `p_raw` pretending to
  be calibrated — and whenever a `p_cal` exists, a `calibration` block rides
  beside it with the method, the fit's ECE raw and calibrated, and the
  fitted-on/held-out counts, so no probability ships without an ECE beside
  it. `GET /_decide/_calibration` publishes the whole reliability curve
  (binned `p_raw` vs empirical frequency with bin counts, raw and
  calibrated), both ECEs, the fit's parameters (isotonic knots or the
  temperature), the pair counts, and the fitted documents' date range; it
  always refits, so it doubles as the operator's refresh (decide requests
  read a per-index cache refreshed at most once a minute). Isotonic is PAVA
  with linear interpolation and end-clamping; temperature is one scalar on
  the log-odds fitted by golden-section NLL search; both are implemented
  from first principles in `xerj_common::calibration` with method citations
  (retrieval skipped: the reference corpora are not present in this
  sandbox). Measured against the FiQA baseline's own retained curve
  (`benchmarks/decisions-calibration`, stdlib Python mirroring the Rust
  exactly — both reproduce the published raw 0.3109 and agree to the fourth
  decimal): raw held-out ECE 0.3831 → isotonic 0.0330 (gate ≤ 0.10 MET);
  temperature 0.3903, honestly failing — one scalar cannot map 0.93 → 0.34
  while keeping 0.05 → 0.0002. Scope of that measurement: bin-level
  held-out (the run retained 10 binned aggregates, not the 19,440 raw
  pairs); the pair-level re-run needs the model's raw scores and is queued
  as the rc.80 final form. `confidence` keeps its raw meaning and the
  abstain gate keeps applying to it.

- **The #1063 gate in its rc.80 final form: pair-level, measured, PASS
  ([#1063](https://github.com/xerj-org/xerj/issues/1063), PR
  [#1087](https://github.com/xerj-org/xerj/pull/1087)).** FiQA re-scored at
  pair level — 19,440 raw (query, doc, `p_raw`, gold) rows, committed —
  instead of the binned reconstruction. Method: BM25 top-30 shortlists
  regenerated on the release binary (`xerj v1.0.0-rc.78`, lexical embedder,
  private port, throwaway data dir) with the harness validating nDCG@10 =
  0.2382 bit-for-bit against the 2026-09-20 baseline before any paid call;
  then the baseline's fixed request shape verbatim (`jev-1.13.0`), 648 paid
  calls (cap 50,000 enforced), 5.06 M input tokens, $0.2125, zero fails.
  Deterministic query-level 80/20 split (`sha256(qid) % 5`, no RNG, no
  clock — #940), deployment-faithful (fit on past queries, apply to new
  ones); pair-level hash split reported as the secondary row. Measured
  (`benchmarks/decisions-calibration/results/2026-09-30-pairlevel-fiqa/`):
  raw all-pairs ECE **0.3109** — the published baseline number reproduced
  exactly on a fresh paid run; held-out raw 0.2872 (Brier 0.1625);
  **isotonic held-out 0.0088 (Brier 0.0256) — gate ≤ 0.10 MET**;
  temperature 0.3163 (T = 1.4589), failing as before. Honest scope, stated
  in the README: ECE flatters on a 96.9 %-negative dataset (92 % of
  held-out pairs calibrate below 0.1); the substance is the Brier halving
  and the top-bin remap — raw 0.90–0.98 becomes 0.24–0.53 against an actual
  held-out relevance rate of 0.4208. The binned-smoothing caveat closes in
  the gate's favour: pair-level (0.0088) beats binned (0.0330), because
  15,840 real pairs give the fit ~98 knots where the binned form gave it 5.

- **`POST /_ask` + `xerj_plan` MCP tool: prompt in, validated query DSL out
  ([#1056](https://github.com/xerj-org/xerj/issues/1056), PR
  [#1076](https://github.com/xerj-org/xerj/pull/1076)).** `POST /_ask`
  (ES-compat) and `POST /v1/ask` (native) take `{index or pattern, prompt}`
  and return `{query, plan, confidence, indices}` where `query` is an ES
  query clause that has ALREADY passed this node's own
  `xerj_query::parse_request` — zero invalid DSL can leave the node, and a
  phrase the index cannot ground is a 422 naming it, never a silent guess.
  The planner is deterministic and rule-based (not an LLM): date literals
  and year phrases, comparator vocabulary, units, scale words and a concept
  lexicon resolve field phrases against the index's real schema; equality
  values resolve against ONE batched `terms` agg (≤1,000 buckets/field,
  ≤24 fields), with a BM25 `match` probe for fields whose values overflow
  the agg. Numeric and date bounds are only ever the prompt's own literals.
  Responses are byte-identical across repeats; `ax-*` patterns route by the
  `autoindex-catalog` dataset description read through the engine's own
  index APIs (no xerj-autoindex dependency in xerj-api). `xerj_plan`, the
  12th MCP tool, proxies it (`index` defaults to `ax-*` like `xerj_search`);
  the published `mcp-tools.json` schema is regenerated. Measured by the
  `benchmarks/ask-plan` harness (PR #1071, unmodified): 230 pairs × 3 runs,
  macro result-set F1 **0.9975** (round 1: 0.9309), `invalid_dsl_out` 0,
  `non_deterministic` 0 — gate ≥ 0.9 over ≥ 200 pairs passed; the three
  sub-1.0 pairs are the "from A through B" date pairs, where this PR reads
  "through" as inclusive by design. ES-YAML on this tree: 1380/0/3. The
  p50 ≤ 300 ms CPU budget is a release-time gate and is not claimed here.
- **The #1056 latency gate measured: `POST /_ask` wall-clock p50 1.129 ms
  against the 300 ms bar (PR
  [#1079](https://github.com/xerj-org/xerj/pull/1079)).** The ask-plan
  harness scored F1 but never recorded milliseconds; `scripts/latency_arm.py`
  is the missing arm (one warmup pass + N measured passes over the same 230
  `pairs.jsonl` prompts on one booted node, per-request wall-clock raw,
  nearest-rank quantiles, stdlib-only, honest stop on endpoint-absence) and
  `run.sh` step 5 now captures it. Measured on the release build of `f0b96d634`
  (private port, throwaway data dir): 690 requests, all HTTP 200, p50
  **1.129 ms**, p90 1.883, p99 2.852, max 155.7 ms — the deterministic
  rule-based planner costs microseconds of CPU, as designed, and the 300 ms
  release bar is met at 266× margin. Results committed verbatim in
  `benchmarks/ask-plan/results/2026-09-30-latency-gate/`; the token arm of
  the same gate remains designed-not-run (live-model cost not approved).
- **Decision cache flywheel: every tier-2+ answer written back, `source` per
  answer, human corrections weighted ≥ 2x
  ([#1061](https://github.com/xerj-org/xerj/issues/1061), PR
  [#1075](https://github.com/xerj-org/xerj/pull/1075)).** `/v1/systemone` and
  `/_decide` now close the loop PR #1072 opened: every tier-2 (local head)
  answer — and every tier-3 answer when that tier exists — is cached into the
  configured `[decisions]` index as an ordinary document carrying the
  configured text/label fields plus `p`, `source` and `ts`, so the answers a
  node computes become the labelled history that answers the next request
  (pinned by test: the same request twice is answered by history from its own
  cached answer). The write-back is spawned, never awaited by the request,
  and cannot fail it — an unreachable index is a log line; a
  configured-but-missing index is created by the write-back itself, which is
  how a local-tier node bootstraps its history. History-tier answers are
  never re-written (they are the index already), and `/_decide` abstains are
  never cached. Every answer now names its tier in a `source` field
  (`/_decide` top level, `/v1/systemone` per-question in
  `decisions.evidence.*.source`) with the strictly-parsed answer objects
  untouched. Human corrections are ordinary history documents carrying
  `human: true` (the existing write path, no new endpoint) and weigh the new
  `[decisions] human_weight`× (default 2.0, the issue's ≥ 2x floor; validated
  positive at config load) their reciprocal rank in the vote — everything
  else keeps the 1/rank arithmetic the published measurements used. The
  issue's Banking77 replay gate and the TypeSafe-terms question on retaining
  provider outputs are release-time gates, deliberately not claimed here.
- **Local `judge` stage on `_search`: `_p_relevant` per hit, `min_p` pruning,
  zero tokens, zero egress ([#1060](https://github.com/xerj-org/xerj/issues/1060),
  PR [#1077](https://github.com/xerj-org/xerj/pull/1077)).** A `_search` body
  may now carry `"judge": {"local": true, "min_p": 0.5}`: the node re-scores
  the hits it was about to return against the question in process — no
  provider, no key, no network call — and each judged hit carries
  `_p_relevant`, a 0..1 relevance probability on an absolute scale. The
  engine's `_score` is NOT replaced (the one deliberate difference from
  `rerank`): a client sorting by `_score` keeps the engine's number while a
  client thresholding `_p_relevant` gets a cut a BM25 score can never give.
  `min_p` drops hits below the threshold and counts them in the response's
  `judged` block (`{applied, scorer, query, judged, kept, dropped, took_ms}`).
  Which judge judged is on the wire: `judged.scorer` is `lexical` or `model`.
  The always-compiled arm is a deterministic lexical scorer (window-local
  BM25 saturation, page-local IDF, K1/B from xerj-fts) with NO semantic
  signal — named as such, never passed off as a model; the model arm (cargo
  feature `decide-local`) reuses the #1072 decide loader and is unmeasured in
  this sandbox — the issue's BEIR bar (0.699 SciFact / 0.345 NFCorpus, FiQA
  ≥ 0.30) and ≤ 40 ms p50 top-30 gates bind at release. Strictness mirrors
  the rerank stage: `sort`/`search_after`/`collapse`/`scroll`/`size:0`/
  `_source:false`-without-fields refused by name before the search runs, and
  every surface that does not run the stage (scroll continuation, `_msearch`,
  the search templates, `_async_search`, `_rank_eval`, the native `/v1`
  search API, the gRPC Search RPC) refuses the block by name instead of
  silently returning unjudged hits under a 200. The MCP tools `xerj_search`
  and `xerj_hybrid_search` take the same `judge` argument (query required on
  hybrid, carried from the plain string otherwise), their descriptions state
  the honest ceiling and the zero-egress difference from `rerank`, and the
  published schema is regenerated.

- **`--decide-mode` / `--decide-model-dir`: the tier-2 decide head is armable
  from the command line (PR
  [#1073](https://github.com/xerj-org/xerj/pull/1073)).** The local decide
  head landed with PR #1072 env-only (`XERJ_DECIDE_MODE` /
  `XERJ_DECIDE_MODEL_DIR`); the server CLI now parses
  `--decide-mode history|local` (default `history`) and
  `--decide-model-dir <path>` with the same flag-wins-over-env shape as
  `--embed-mode`, resolving through the same `DecideSettings::resolve` into
  the `state.decide` seam (xerj-api unchanged). Also adds the `decide-local`
  cargo feature to xerj-server (off by default, matching xerj-api's gating) —
  the rebuild `resolve`'s own warning text had been naming all along. A
  default build asked for `--decide-mode local` boots on the history vote and
  says exactly which rebuild arms the tier; verified by booting both builds
  (armed/off/missing-dir/mixed flag+env).


- **Local zero-shot decision head — tier 2 of the System One decide ladder
  ([#1057](https://github.com/xerj-org/xerj/issues/1057), PR
  [#1072](https://github.com/xerj-org/xerj/pull/1072)).** `/v1/systemone` and
  `/_decide` now answer through an ordered ladder per question: the history
  vote wins wherever it has labelled support (unchanged); a local
  ModernBERT-class candle classifier — cargo feature `decide-local`, armed by
  `XERJ_DECIDE_MODE=local` + `XERJ_DECIDE_MODEL_DIR` — answers every
  no-support class (no `[decisions]` index, a missing/unsearchable one, no
  labelled neighbour), so the same request that 503'd/422'd answers 200 and
  tiers mix per-question in one response; a hosted key stays reserved. The
  head scores NLI (premise, hypothesis) pairs per candidate label and
  renormalises, loads from a local model directory only (no hf-hub, no new
  outbound client — the egress inventory is unchanged), echoes its own id
  `xerj-decide-local-1` (never a Jev name), and fails loudly (503 naming the
  directory) rather than fabricating a probability. Default builds keep the
  exact previous 503/422 contract. Loader validated end to end against a
  deterministic test fixture; no accuracy/ECE/latency number is claimed until
  the trained `xerj-decide` artifact (#1064) is measured at release.

- **Token-budgeted MCP responses: `max_tokens` on every search tool
  ([#1058](https://github.com/xerj-org/xerj/issues/1058)).** The five MCP
  search tools (`xerj_search`, `xerj_hybrid_search`, `xerj_semantic_search`,
  `xerj_vector_search`, `xerj_code_search`) returned whatever the engine
  sent, so an agent that asked for 50 hits got all 50 verbatim — context
  cost unknown and unbounded. All five now take `max_tokens` and are held to
  it: the response text NEVER exceeds the budget, enforced by construction
  (a note reserve is subtracted before any content is admitted) and pinned
  by tests across a range of budgets on both response surfaces. Tokens are
  counted with one named tokenizer — XERJ's `StandardTokenizer` (UAX #29
  word boundaries, the same word split the engine's analyzer uses for BM25;
  `unicode-segmentation` depended on directly so the count is identical by
  construction without pulling the FTS stack into the thin proxy) — an
  honest proxy for model-context tokens that under-counts subword splits,
  stated in the argument's description. Overlapping file:line passages are
  deduped before budgeting (higher-ranked shown passage wins); hits that no
  longer fit are returned as locators only (file:line, index/id — the
  citation, never the passage) while they fit, and a `_token_budget`
  accounting block reports kept, locator-only, deduped, dropped, and
  `tokens_used`. The engine never sees the argument (request bodies are
  bit-identical), errors are never truncated, nothing binding returns the
  engine text byte-identical, and a budget below 32 (not even the envelope
  fits) is a builder error, never a silent drop. Generalises
  `xerj_code_search`'s char-level `full` cap to the whole response. The
  published schema `landing/docs/agents/schemas/mcp-tools.json` was
  regenerated from the binary; `published_schema_drift` stays green. (PR
  [#1067](https://github.com/xerj-org/xerj/pull/1067).)
- **`xerj_map` MCP tool: per-index field map from the autoindex catalog
  ([#1055](https://github.com/xerj-org/xerj/issues/1055)).** The catalog
  index (`autoindex-catalog`) holds, per dataset, the full `FieldSpec` list
  the profiler inferred, but only the CLI could read it — an MCP agent
  writing query DSL had to guess field names and ate unknown-field 400s on
  the searches it then issued. The twelfth MCP tool reads the catalog (the
  same `doc_kind:dataset` query `xerj autoindex map` runs, with
  `sample_queries_json` projected out of `_source`) and renders, per index:
  record count, time field and range, semantic body field, and every field —
  name, `es_type`, date encoding, semantic tag, cardinality estimate,
  coverage, up to 5 examples, and the sampled date AND numeric min-max.
  Default response ≤ 4 KB per index (`max_bytes`, floor 1024); the documented
  trim ladder cuts examples to one, then examples, then per-field notes, then
  lowest-coverage fields, with `trimmed`/`fields_omitted` accounting for
  every cut. An exact `index` name filters engine-side, a `*`-glob
  client-side; a missing catalog is an error naming the fix, an empty one a
  normal empty result. `FieldSpec` itself gained the numeric range
  (`num_min`/`num_max` from the `int_min`/`int_max` `FieldAcc` has tracked
  all along) — populated exactly on `long` fields, deliberately absent on
  `double` (the accumulator tracks only the i64 subset, so a fractional
  column would carry its integers' range stated as fact), with
  `#[serde(default)]` so every pre-existing catalog `fields_json` and state
  plan still deserialises. Published schema regenerated (12 tools);
  `published_schema_drift` stays green. (PR
  [#1074](https://github.com/xerj-org/xerj/pull/1074).)
- **`_watcher` is real: `PUT /_watcher/watch/<id>` now either evaluates a
  watch on schedule or refuses it with 501
  ([#1062](https://github.com/xerj-org/xerj/issues/1062)).** The route used
  to answer 201 `"active": true` to every body with no machinery behind it —
  accepted-and-ignored, the #204 class at route level — and the alert
  indices `.xerj_alert_rules` / `.xerj_alert_fires` were written by nothing.
  One watch type is implemented: `trigger.schedule.interval` (>= 1s),
  `input.search` over exactly one index, `condition.xerj_decide` (question,
  positive_label, p_min, k, optional decide index) and an `index_alert`
  action. A per-watch background task prefilters candidates with the watch's
  own query (ascending `_seq_no` keyset paging, 100 per page — the #1022
  shape), runs each rendered question through the real `/_decide`, and writes
  a fire record into `.xerj_alert_fires` when the positive label wins
  non-abstaining at or above `p_min`. Anything else (cron schedules,
  transforms, other action types, multi-index input) is refused with a 501
  that names what is not evaluated; malformed supported shapes are 400. Two
  cursor subtleties are pinned by tests: the empty-index baseline is `-1`
  because `_seq_no` is 0-based on the wire (a 0 baseline strands the first
  document ever indexed), and the baseline is captured in `put_watch` before
  the evaluator spawns so documents indexed between the 201 and the first
  poll cannot be absorbed unjudged. Fire records carry the RAW decide
  confidence, plus `p_cal` with `calibrated: true` when the node has a
  fitted calibration (#1063, PR #1080) — the threshold stays on the raw
  value. Honest limits: a restart does not resume watches (evaluators are
  spawned by `put_watch` only), and the per-pass decide budget is 1000 with
  the cursor held below a failure so the backlog is retried, never dropped.
  Zero watches cost zero tasks; an idle watch's tick measured sub-second
  (sub-millisecond, pinned by test). (PR
  [#1082](https://github.com/xerj-org/xerj/pull/1082).)
- **`xerj autoindex <folder> --label <question-set.json>` labels each
  document at ingest through `POST /_decide`
  ([#1062](https://github.com/xerj-org/xerj/issues/1062)).** The
  question-set file (the `noul`/`choice` vocabulary `/v1/systemone` already
  uses) is validated whole before the folder is walked; `{{field}}` in a
  question renders against each record's own fields. A `noul` is one binary
  vote; a `choice` is one vote per option with argmax over independent
  binary votes (an option that lost its vote contributes `1 - confidence`).
  Fields written: `label`/`label_p` for a single-question set, plus
  `label_<id>`/`label_<id>_p` per question. Abstention is an answer —
  `label: null` with the raw p carried for later calibration; `p` is the
  RAW decide confidence everywhere. A FAILED decide fails the run before the
  document is bulk-indexed: an index the operator believes is labelled is
  the #204 defect class. The vote runs at both record sinks — the
  generated/durable route decides inside `prepare_artifact` before sealing
  (labels are part of the durable artifact; replays and #971 hardlink reuse
  carry them without a second vote, and the question-set bytes are part of
  `prepared_records_identity`, so a changed set re-prepares instead of
  hardlinking differently-labelled NDJSON) and the legacy phase-B sink at
  the same point. `--label` is refused by name on every route that cannot
  apply it (folder `--watch`, object-storage watch, `map`, `status`,
  `--dry-run`). Cost is ingest-time only — one round trip per noul (per
  option, for a choice) per record; with the flag absent no `Labeler` is
  constructed, so idle cost is zero by construction and the measured cost
  (ms/doc, printed by the end-to-end test against a loopback node) is the
  floor, not the ceiling. (PR
  [#1082](https://github.com/xerj-org/xerj/pull/1082).)
### Fixed

- **Main's `Build + Test` un-reds: #1062's label test stubs are classified
  in the egress inventory (PR
  [#1083](https://github.com/xerj-org/xerj/pull/1083)).** The merge of #1082
  added two unclassified outbound-client files
  (`xerj-autoindex/src/label.rs`, `label_http_tests.rs`) that the author's
  test scope never exercised — the xerj-rerank guard
  `every_outbound_client_in_the_engine_is_on_the_published_list` runs in a
  different crate's suite and only CI saw it. Both are `Role::TestOnly`,
  verified rather than asserted: `label.rs`'s only marker (`TcpStream::connect`,
  the DecideStub drop-wakeup) sits after its `#[cfg(test)]`, and the
  production Labeler decides through the shared `Es` client already on the
  published list — no new egress path exists, so `docs/RERANK.md`,
  `xerj_rerank::DATA_EGRESS` and the air-gapped recipe are unchanged.
- **PAVA knot-index bug in the calibration benchmark's Python mirror
  (PR [#1087](https://github.com/xerj-org/xerj/pull/1087)).**
  `benchmarks/decisions-calibration/fiqa_gate.py` recorded each pooled
  block's first-x as `len(blocks)` at append time, which stops equaling the
  pooled-point index after the first backward pool — every knot created
  after a violation got its x shifted down and `apply()` mapped
  probabilities through a compressed curve. Invisible on the bin-level gate
  data (its even-bin fit points are strictly increasing, so no pooling
  occurs and buggy and correct code coincide — both reproduce 0.0330, which
  is why the Rust pinning test stayed green); it corrupted any fit on data
  with local violations, i.e. the pair-level rerun, where the first pass
  produced a nonsense isotonic 0.2372. The shipped Rust module
  (`xerj_common::calibration`) was audited and is correct — blocks carry the
  `enumerate()` pooled index and expand means to every pooled point — so
  only the mirror diverged and no engine change was needed. The fixed
  mirror matches the Rust representation exactly, agrees with an
  independent naive run-pooling PAVA to 8.7e-19 at all 98 pooled points,
  and still reproduces the bin-level 0.0330 to the digit. Lesson recorded:
  the pinning test asserted ≤ 0.10, which the wrong number also satisfied —
  an exact-value pin on a pooling input would have caught the divergence.
- **Release notes now compare against the previous engine release instead of
  the latest corpus pack.** GitHub anchors auto-generated notes ("Full
  Changelog") on the most recently published release, so since the daily
  `pack-rust-vulns-*` scheduled releases started, every cut's compare base
  drifted: `v1.0.0-rc.78` shipped comparing
  `pack-rust-vulns-2026-09-29...v1.0.0-rc.78` and had to be hand-patched via
  REST. The release job now computes the base itself (highest `v[0-9]*` tag by
  version sort that is not the tag being cut) and pre-renders the notes with an
  explicit `previous_tag_name`, falling back to auto-generation only when no
  previous v-tag exists (PR
  [#1069](https://github.com/xerj-org/xerj/pull/1069)).
- **Release notes: the pinned compare base now orders a final above its own
  rcs, and the derivation is fixture-tested.** The PR #1069 pin derived the
  base with plain `--sort=-v:refname`, which orders `v1.0.0-rc.79` above
  `v1.0.0` — the opposite of what that PR's message claims — so the first
  `v1.0.1` cut would have compared against `v1.0.0-rc.79` rather than
  `v1.0.0`, and re-dispatching an old tag would have compared against the
  newest tag instead of that release's own predecessor. The derivation now
  lives in `.github/scripts/prev_tag.sh`: `versionsort.suffix=-rc` puts a
  prerelease below its final, the base is the version-successor of the tag
  being cut, and a five-assertion fixture self-test (pack-tag noise,
  `rc.9` created after `rc.78`, final among rcs, old-tag rebuild, first-ever
  cut) runs in the release job before the base is trusted. Measured against
  the live tag set on 2026-09-30: a hypothetical cut selects
  `v1.0.0-rc.78`, while GitHub's auto-detection would anchor on
  `pack-rust-vulns-2026-09-30`, published 10:47Z that morning (PR
  [#1081](https://github.com/xerj-org/xerj/pull/1081)).
- **`benchmarks/ask-plan/` — the harness behind issue #1056's gate, shipped
  before the endpoint so the gate is designed in the open**: 230 (prompt,
  gold result set) pairs over three committed public tabular snapshots
  (USGS earthquakes — public domain; NASA Exoplanet Archive `pscomppars` —
  acknowledgment given, statutory basis stated; Gapminder — CC BY 4.0), with
  gold derived only by filtering raw rows in Python (never by running an
  engine), deterministic stdlib-only derivation/verification scripts, a
  loader + fixture self-check, the `POST /_ask` scoring arm (macro result-set
  F1, zero-invalid-DSL, 3-pass determinism), and the guarded two-arm
  `claude -p` comparison the issue specifies. Validated end-to-end against a
  local rc.78 build: all 230 `query_equivalent`s reproduce their gold sets;
  with `POST /_ask` absent the harness stops at `measured: false`. The
  directory contains zero measured numbers by design — every F1/token cell
  in its README is a gate definition until a real run lands (PR
  [#1071](https://github.com/xerj-org/xerj/pull/1071)).
### Changed

- **The shipped release binary is built WITH the `decide-local` cargo
  feature — tier 2 arms on the stock binary (PR
  [#1097](https://github.com/xerj-org/xerj/pull/1097)).** The rc.80 gate
  runs caught that `--decide-mode local` on the rc.78 release binary logged
  a rebuild warning and stayed off: xerj-api's `decide-local` feature was
  never enabled in xerj-server's release build, so the tier shipped absent,
  not merely flagged off. `decide-local` is now a DEFAULT feature of
  xerj-server (default-on, deny-by-default at RUNTIME: the tier activates
  only on the explicit `--decide-mode local` / `XERJ_DECIDE_MODE=local`, so
  every default boot keeps the exact previous behaviour), release.yml builds
  it into the published assets, and the artifact loader is exercised by the
  release smoke path. The catch and its queue-ahead status are stated in the
  blog post; this is the fix it pointed at.
- **`xerj autoindex` now creates prose indexes with a stemming analyzer
  ([#1059](https://github.com/xerj-org/xerj/issues/1059)).** When the
  profiler marks a dataset's prose semantic — the same measured predicate
  that gates the semantic-body election (`word_ratio >= 0.55 &&
  mean_tokens >= 3.0`; measured `word_ratio` 0.00 for `trace_id`-shaped
  identifiers, 0.78–1.00 for prose) — the index CREATE body declares the
  built-in Snowball English `stemmer` analyzer as
  `settings.analysis.analyzer.default`, the surface #937/#991 made
  honoured at flush, segment query and merge. Plurals and inflections now
  match on the lexical arm (`databases` ↔ `database`); `stemmer` was
  chosen over `english` because it adds no stop-word removal and so
  changes no BM25 length norm. Identifier/enum/numeric datasets keep the
  exact `standard` term space, keyword fields are never stemmed, and
  existing indexes are untouched by construction (analysis settings are
  immutable after creation and a CREATE that finds the index already
  there does not re-send). The declaration rides the plan outside the
  frozen identity hashes, so no existing state dir aborts on upgrade.
  The BEIR/NFCorpus zero-hit measurement (7 of 323 NFCorpus queries
  return no hits on plural-only mismatches) remains the release-time
  gate. (PR
  [#1070](https://github.com/xerj-org/xerj/pull/1070).)

### Documentation

- **The rc.80 gates post on the blog: "The rc.80 gates: three passed
  outright, three failed, one split" (PR
  [#1096](https://github.com/xerj-org/xerj/pull/1096)).** The blog half of
  the ten-item program's packaging rule (#1055–#1064, discussion #1054):
  every gate verdict, wins and losses, each figure traced to the gate run
  that produced it. PASS: `/_ask` macro-F1 0.9975 over 230 pairs, 0 invalid
  DSL, 3-run determinism, wall-clock p50 1.129 ms against the 300 ms bar
  (#1076, #1079); `xerj_map` unknown-field 400s 20/30 guessed → 0/30
  map-informed, 100% against the ≥80% bar (#1074, #1086); calibration FiQA
  pair-level ECE 0.3109 → 0.0088 held-out against the ≤0.10 bar with the
  temperature arm's 0.3163 published as the failing comparison (#1080,
  #1087). FAIL, published as such: the lexical judge loses to its own first
  stage everywhere measured (0.7045→0.5967, 0.3419→0.2924,
  0.2382→0.1650 against the 0.30 FiQA bar) and stays opt-in with no quality
  claim per its own rule (#1077, #1090); the flywheel freezes as shipped
  (12 of 2,000 cached, #1094) and forces 19.72% coverage at 0.9953 accuracy
  against the 80%/0.97 bars (#1075, #1088); the tier-2 decide head misses
  SMS 0.9193/0.287 against 0.95/0.05 and AG News 0.2599 against 0.85, with
  latency 8.4 ms passing, and ships behind the flag with the numbers
  published (#1072/#1073, #1088). SPLIT: stemming +0.0160/+0.0179 BM25
  nDCG@10 passes while zero-hit 25→15 fails the ≤10 bar — the 15 have no
  stem anywhere in the corpus (#1070, #1085). The post also tells the gate
  catch that the stock release binary was built without the `decide-local`
  cargo feature (the flag a no-op, `stock-binary-noop.txt`) with the fix
  queued ahead of the cut rather than claimed done, and states the honest
  gaps: the `/_ask` token arm designed not run, the `_watcher` detection
  gate not yet measured, no Hugging Face URL claimed for `xerj-decide-v1`.
  Mechanics: hand-written `landing/blog/the-rc80-gates.html`, registered in
  `scripts/seo/pagedata.py` (published/updated 2026-09-30), card prepended
  to the blog hub with its `dateModified` bumped, head/footer owned by the
  seo generators, sitemap regenerated post-commit; all local gates green
  (build_articles/gen_sitemap/fix_heads/fix_links/mk_og_card `--check`,
  `seo_lint` 0 violations, `factcheck` 0 errors, constants guard).
- **Measured the #1059 stemming gate on the release binary — the BM25 half
  passes, the zero-hit half fails at 15 (PR
  [#1085](https://github.com/xerj-org/xerj/pull/1085)).** Paired arms on one
  node (`benchmarks/beir-hybrid`, release binary @ `a179e1ef3`): the same
  create-time `settings.analysis.analyzer.default` = `stemmer` surface
  #1070 ships, vs the standard-analyzer baseline, which reproduced the rc.74
  row exactly (0.6572 SciFact / 0.3016 NFCorpus / the same 25 zero-hit
  queries). BM25 nDCG@10: SciFact 0.6572 → 0.6732 (+0.0160) and NFCorpus
  0.3016 → 0.3195 (+0.0179) — both clear the gate's +0.01, so stemming is a
  measured BM25 win, not a regression. NFCorpus zero-hit queries went 25 →
  15, not the required ≤10: stemming repairs exactly the 10
  plural/inflection queries; the remaining 15 are single words (Fosamax,
  Zoloft, eggnog, Yale…) whose stem also occurs nowhere in the corpus — not
  bridgeable by any stemmer, the vector arm remains their only repair. Raw
  per-query outputs in
  `benchmarks/beir-hybrid/results/2026-09-30-stemming-1059/`. Also recorded:
  `GET _settings` does not echo the analysis block and `_analyze` shows the
  standard path even when the stemmer is declared and provably applied by
  search.
- **Measured the #1060 local-judge gate on the rc.80 release binary: the
  lexical judge LOSES, and the number ships (PR
  [#1090](https://github.com/xerj-org/xerj/pull/1090)).** New BEIR harness
  `benchmarks/beir-hybrid/judge_gate.py` (same qrels/nDCG@10 scoring as
  `eval.py`, 3 runs per arm, shuffled order, size-30 pages, judge carrying no
  `min_p` — reorder-only) on `xerj v1.0.0-rc.78` with the neural embedder,
  node pinned to 8 cores: hybrid+judge **0.5967** SciFact vs hybrid **0.7045**
  (−0.1077), **0.2924** NFCorpus vs **0.3419** (−0.0495), bm25+judge
  **0.1650** FiQA against the 0.30 bar (hosted Jev rerank: 0.3638) — the
  quality gate FAILS on all three rows and the stage stays opt-in with no
  quality claim, per its own "loses, does not ship" rule; nothing was tuned
  or re-rolled. The cost AC passes with room: **+0.7 / +0.5 / +0.6 ms** added
  p50 for a judged top-30 page (server-side `judged.took_ms` p50 0 ms). Every
  arm reproduced to the fourth decimal across runs AND across a node restart
  (post-#940 determinism), and the BM25 arms reproduce the 2026-09-18
  baselines bit-for-bit (0.6572 / 0.3016 / 0.2382), validating the harness.
  Also recorded: hybrid on the 57,638-doc FiQA index costs 13-26 s/query
  (14.7-15.1 s server-side), and an informational `min_p: 0.5` arm empties
  274 of 300 SciFact pages — the lexical scale is not thresholdable at round
  numbers. Raw logs and sha256s:
  `benchmarks/beir-hybrid/results/2026-09-30-judge-gate/`.
- Rolled `ROADMAP.md`'s tracker state after the issue tracker was emptied on
  2026-09-29, the day of the rc.78 cut: the four issues still open after the
  cut — #1038 and #1030 (their shipped halves are the rc.78 record below) and
  the defects #1031/#1032, closed as deferred, not fixed — were closed, the
  abandoned pointer #298 was unpinned, "In flight" emptied, the open-defects
  shortlist became a plain-text deferred record (the release-notes gate checks
  that section's issue links against the live tracker at every cut), and the
  corpus-hub and mail-ingest lines state the closed statuses (PR
  [#1053](https://github.com/xerj-org/xerj/pull/1053)).
- Filed the ten-item program to the first official release
  ([discussion #1054](https://github.com/xerj-org/xerj/discussions/1054)) as
  issues #1055–#1064 with acceptance criteria, measured gates, and code refs
  verified against the tree (two ref paths corrected; the plan's rc.78 targets
  slid to rc.79 — rc.78 shipped before the discussion posted); created
  milestones rc.79/rc.80/v1.1.0 and labels (mcp, agents, autoindex, search,
  systemone, zero-token, roadmap); stood up the pinned plan tracker
  [#1065](https://github.com/xerj-org/xerj/issues/1065); rolled ROADMAP's
  *In flight* and GA-program sections to match and pointed the two zero-token
  items it subsumes at their new trackers (PR
  [#1066](https://github.com/xerj-org/xerj/pull/1066)).
- **llms.txt / llms-full.txt tool inventory now matches the released
  thirteen-tool MCP registry (PR
  [#1078](https://github.com/xerj-org/xerj/pull/1078)).** Both files said
  `mcp-tools.json` holds "ten tools" and llms-full's own verify example
  printed `jq '.tools | length'` → 10 while the real registry is thirteen
  (`xerj_map` and `xerj_plan` landed without the agent-facing docs
  noticing) — the llms-txt study's §8 lesson repeating verbatim. Hand-written
  regions only: the mcp line names all seven additions over the HTTP-core
  six, the anthropic/openai lines state the composition by design,
  `POST /_ask` enters the endpoint map (it was undocumented in either file),
  and llms-full §3 is retitled THE CORE AGENT OPERATIONS with the jq example
  pinned to 13. Checked, not asserted: all 13 names from the published
  `mcp-tools.json` (pinned to `tool_specs()` by `published_schema_drift`)
  appear by name across the two files.
- **Console UX review: the post-indexing surface against the user gate, in
  the open (PR [#1089](https://github.com/xerj-org/xerj/pull/1089)).**
  Method stated first because the sandbox has no browser: the served SPA
  fetched from a running node, its API calls curled verbatim under a REAL
  operator session (first-launch bootstrap completed — magic link from
  stderr, passkey enrolment with a software P-256 authenticator), and the
  repo's own pure renderers executed under node against the live payloads,
  so quoted screens are the SPA's output, not a paraphrase. Corpus: 72 mixed
  files (mbox, .eml, CSV, NDJSON, wikilinked notes, source) → 8 datasets,
  512 records, 290 edges in 2.2 s on rc.78. The three gate questions — does
  the console show how large the corpus is, what data is there, what XERJ
  can do — answered per screen with the defects named, and its verdict
  (the console answered volume but guessed structure) is what the
  knowledge-surface change in this release (#1095) was cut against. No
  engine code changed.
- Rolled the corpus-pack status in the agent-facing index files
  `landing/llms.txt` and `landing/llms-full.txt`, which still said
  "planned, no code": the records half shipped in rc.78 — `xerj corpus
  build`, per-file checksums, the detached ed25519 signature over
  SHA256SUMS, the daily dated `rust-vulns` release — while stating
  plainly that the pre-indexed half and the hub directory remain
  unbuilt, and linking `docs/CORPUS_PACKS.md` as the proving file (PR
  [#1068](https://github.com/xerj-org/xerj/pull/1068)).

## [1.0.0-rc.78] - 2026-09-29

The index-size and corpus-packs release. Two headline changes: stage 1 of the
index-size epic [#1038](https://github.com/xerj-org/xerj/issues/1038) lands four
measured format changes (ZNV2, ZPS2, ZBS4, ZFM5 — the committed same-day pair
5,398,478 → 3,989,254 B durable on the 100k harness, −26.1 %), and
`xerj corpus build` ships portable, signed corpus packs with the first showcase
pack, `rust-vulns`, published from a scheduled workflow. The same window closes
the idle-cost arc (#874 met at ~3× margin, idle 206 → 64 kB per index) and the
by-query truncation class (#1019, #1022).

### Fixed

- **An idle node no longer spends CPU per index on Prometheus gauges (#874).**
  The 10 s background loop that fed `xerj_doc_count` / `xerj_segment_count` /
  `xerj_wal_size_bytes` / `xerj_memory_usage_bytes` walked every index's WAL
  subtree on an async runtime worker on every tick — one `read_dir` plus one
  `stat()` per WAL shard (~16 per index at default sharding) whether anyone
  read the values or not. The new at-rest fixture
  (`benchmarks/idle-budget/`) measured that at ~0.7–0.9 % of one core for 450
  idle indices (against #874's < 0.5 % budget), entirely in the `xerj-rt`
  workers; deleting the loop and refreshing the gauges at scrape time in the
  `/v1/metrics` handler (the same way the query-cache gauges already
  reconcile) drops idle CPU to 0.15–0.18 % and wakeups from ~46/s to ~26/s on
  the same corpus, with the WAL walk moved to the blocking pool. A scrape is
  the only moment these gauges are observable, so nothing is lost between
  scrapes. The fixture and its coarse CI gate (`idle-budget` job, calibrated
  to 256 kB/idx on the runner's host shape) keep the budget honest: CPU < 0.5 %
  of one core, O(1) wakeups/s, boot-to-green with zero WAL replay on a
  cleanly-flushed corpus. The 0.2 MB/index product line itself was still at or
  above the line on 4-vCPU hosts at this point (204.9–209.9 kB measured,
  N-slope 206.5 kB/idx) — that is
  [#1024](https://github.com/xerj-org/xerj/issues/1024), met only after the
  lazy seen-set work below (~63–69 kB, >3× margin). (PR
  [#1020](https://github.com/xerj-org/xerj/pull/1020).)
- **The flush publication bracket no longer spans the drain**
  ([#1015](https://github.com/xerj-org/xerj/issues/1015)): flush drains now
  freeze and the bracket wraps only the publish, so the ms-scale bracket
  constraint the #1014 evenness check enforces finally holds on the drain side
  too. Reader-fairness part two. (PR
  [#1018](https://github.com/xerj-org/xerj/pull/1018).)
- **Large-ingest id-position maps are built from the `__id` projection, and the
  stored reassembly is streamed** ([#950](https://github.com/xerj-org/xerj/issues/950))
  — the attribution work closed the issue rather than filing a follow-up: the
  per-index structures that grew with corpus size during ingest were rebuilt
  around projections and streaming, with `POST /{index}/_cache/clear` added to
  release the rebuildable caches on demand (PR
  [#1017](https://github.com/xerj-org/xerj/pull/1017); the cache endpoint is PR
  [#1009](https://github.com/xerj-org/xerj/pull/1009)).
- **`_delete_by_query` pages the whole match set, ids-only**
  ([#1019](https://github.com/xerj-org/xerj/issues/1019)) — the one-shot
  `size: 10_000` truncation class found there is gone from the delete path.
  (PR [#1021](https://github.com/xerj-org/xerj/pull/1021).)
- **`_update_by_query` pages the match set, with exact sig-text/enrich counts**
  ([#1022](https://github.com/xerj-org/xerj/issues/1022)) — closing the
  remaining single-shot by-query sites the issue named in the #1019 truncation
  class; a single request is now a full-corpus traversal with a 10,000,000
  `max_total` backstop rather than a silent 10,000-hit cap. (PR
  [#1023](https://github.com/xerj-org/xerj/pull/1023).)

### Added

- **`xerj corpus build` — portable, signed corpus packs, and the first one:
  `rust-vulns`.** The records half of the corpus-hub roadmap item (#1030): a
  declarative TOML recipe (sources with licences, identity edges, merge
  precedence, derived fields — the tool is domain-agnostic; what a record *is*
  lives in the recipe, never the code) drives harvest → normalize →
  content-addressed store (file presence is the skip-already-stored dedup —
  incremental by construction, no sync mode to get wrong) → union-find
  identity resolution (alias chains close for free) → merge (declared scalar
  precedence, array union, most-restrictive licence, auditable `sources[]`)
  → a deterministic sharded pack whose manifest carries a `format_version`
  readers refuse when unknown, per-file SHA-256 verified on
  `xerj corpus add --from <dir|zip>`, the recipe verbatim as provenance, and
  mapping/relation *suggestions* that are never applied. The showcase pack
  `tools/packs/rust-vulns/` collapses 4,107 osv.dev + RustSec filings to
  1,950 identity-resolved records (1,221 merged from both sources) carrying
  RustSec's curated vulnerable-function paths, which no export ships; every
  README number is measured (`pack-stats.json`, `provenance: measured`) and
  pinned by a test. Publishing is a separate step, never part of a build:
  `xerj corpus keygen`/`sign` write a raw 64-byte ed25519 detached signature
  over SHA256SUMS (the public key travels beside the recipe, never inside the
  pack), `corpus add --verify-sig <pubkey>` checks origin *before* anything is
  materialized, and a scheduled workflow rebuilds, signs, and publishes the
  pack to a dated GitHub Release per build day (immutable releases accept
  asset uploads only before a release is published — so the workflow uploads
  to a draft and publishes it last — and make release tags single-use, so
  there is no one rolling URL; the draft-then-publish mechanics are
  [#1049](https://github.com/xerj-org/xerj/pull/1049), the dated
  per-build-day tags [#1050](https://github.com/xerj-org/xerj/pull/1050)) —
  verifying its own output against the
  committed `.pub` first, so a half-rotated key fails the build. The attack
  the signature exists for is pinned by a test: a self-consistent rebuild
  (tampered records, honestly rewritten checksums) passes every checksum and
  fails the signature. No vulnerability database in our prior-art survey
  ships a consumer-verifiable signature. (PRs
  [#1046](https://github.com/xerj-org/xerj/pull/1046) and
  [#1047](https://github.com/xerj-org/xerj/pull/1047) for the builder and
  showcase; the signing/publish half is
  [#1048](https://github.com/xerj-org/xerj/pull/1048); the loose
  `<pack>-SHA256SUMS.sig` release asset that `--verify-sig` requires beside a
  pack zip — without which the documented zip consumer flow failed — is
  [#1051](https://github.com/xerj-org/xerj/pull/1051).)
- **An on-disk size harness for force-merged indexes**
  (`benchmarks/index-size/`) — the measurement half of the index-size
  effort (epic [#1038](https://github.com/xerj-org/xerj/issues/1038)):
  `run.sh` clones the DISK_SIZE_2026-07-09 protocol (chat-events cycled to
  100k docs enriched with a deterministic text `body` and a
  high-cardinality `doc_id`, explicit mapping, forcemerge 1,
  du-stable-30s) and reports a per-extension and per-field byte breakdown
  of everything durable except the WAL; `LEVEL=best` re-runs at the
  merge-path compression knob. CI prints a 20k-doc pass informationally on
  every PR via the existing api-checks job. `DESIGN.md` records where the
  bytes are today (with file:line anchors), the peer designs adapted
  (tantivy columnar/sstable/postings, quickwit docstore), and the staged
  plan: header/delta wins inside existing envelopes → FTS-dictionary reuse
  → zstd trained dictionaries. No engine format changed yet; numbers from
  this harness gate the stages that do.
- **`POST /{index}/_cache/clear`** releases the index's rebuildable caches
  ([#950](https://github.com/xerj-org/xerj/issues/950) follow-up work). (PR
  [#1009](https://github.com/xerj-org/xerj/pull/1009).)
- **An email-labelling benchmark for the System One wire**
  ([discussion #1012](https://github.com/xerj-org/xerj/discussions/1012)):
  local `POST /_decide` measured on 260 originally-authored synthetic emails
  across the discussion's own four labels — 1.000 accuracy on the templated
  tier, but 0.625 at 0.902 mean confidence on a hand-authored
  boundary-crossing tier: wrong **and** confident, the miscalibration shape the
  discussion itself flags for the hosted path. Nothing is wired into
  `autoindex`; the README lists what that would still need. (PR
  [#1026](https://github.com/xerj-org/xerj/pull/1026).)

### Performance

- **`.meta` term records go columnar-varint (ZFM5)** ([#1038](https://github.com/xerj-org/xerj/issues/1038)):
  the per-term `{df u32, ttf u64, offset u64, len u32}` records under the
  ZFM4 zstd envelope kept a fixed 24-byte stride, and zstd cannot match
  across the stride's alternating field widths — on the 100 k-doc size
  harness `doc_id.meta` (100 k unique terms, every one `ttf == df`, every
  postings gap zero) still cost 138,727 B on disk for ~200 B of
  information.  ZFM5 re-shapes the section before compression into four
  columnar varint streams — `df`, `ttf − df` (elided entirely when every
  term in the field has `ttf == df`, the keyword-field shape), `offset
  gap`, and `postings_length` — so zstd sees long runs of one-symbol
  lanes.  The reader expands the streams once at open time into the same
  in-memory flat-record array ZFM3/ZFM4 build, leaving the FST-offset
  lookup hot path untouched.  The encoder fails closed on non-monotone
  postings offsets (gaps reconstruct by accumulation, so a negative gap is
  unrepresentable) and the decoder rejects truncated streams, trailing
  stream bytes, and length mismatches.  Peer pattern: tantivy's
  `TermInfoValueWriter::serialize_block` (`src/termdict/sstable_termdict/
  mod.rs:92-105`, MIT) writes block-leading absolute range starts once
  and then per-term length varints only.  Measured on the 100 k-doc
  harness (LEVEL=balanced, forcemerge 1, same-day control): the `.meta`
  family drops 200,826 → 13,774 B (−93.1 %), total durable −3.5 %, with
  every other extension byte-identical except 3 B of `.jsonl` state noise;
  the win scales
  with term cardinality, which is exactly the shape of unique-id fields
  on large deployments.  Measured again on a tree carrying ZBS4 as well
  (`results/result-zbs4-zfm5-balanced.json` vs
  `result-zbs4-only-control-balanced.json`, same day): 4,176,306 →
  3,989,254 B — the two stages together take the pre-stage baseline
  5,398,478 → 3,989,254 B (**−26.1 %**), with the four staged deltas
  (ZNV2, ZPS2, ZBS4, ZFM5) summing to within 12 B of the directly
  measured endpoint.  ZFM4-and-older segments decode unchanged. (PR
  [#1043](https://github.com/xerj-org/xerj/pull/1043).)

- **Stored sections get typed-int columns, duplicate-column references and
  a merge-path zstd effort chooser (`ZBS4`)** — stage 1 of the index-size
  epic [#1038](https://github.com/xerj-org/xerj/issues/1038), redesigned
  by measurement: dissecting a harness `.seg` showed the planned
  dict-stream work targeted ~5 % of the section while the `body` RAW_JSON
  fallback alone was 84.2 %, `__seq_no` was stored as textual consecutive
  integers, and `doc_id` was a byte-identical duplicate of `__id`.  Three
  codecs instead (`engine/crates/xerj-storage/src/stored_codec.rs`):
  **TYPED_INT** stores an all-integer column as a presence bitmap plus
  128-value frame-of-reference bit-packed blocks (`[width][zigzag-varint
  min]`, width 0 = constant block — the ZPS2 `.post` shape) with a
  zigzag-varint delta residual; blocks whose spread exceeds u32 lanes keep
  the JSON fallback, floats/strings never qualify.  A dict-eligible
  integer column races the two codecs and keeps the smaller (the Parquet
  pick-the-smallest rule the `.dv` ZNV2 chooser uses).  **COPY_OF** stores
  a later column byte-identical to an earlier pass-1 column as a 4-byte
  reference (digest + full-equality verify before writing), so an id echo
  in `_source` costs 4 B instead of a second full payload.  **The
  long-effort chooser** — merge path only, flush stays pinned to zstd 3
  after the ingest regression — additionally compresses each JSON fallback
  column at level 19 with long-distance matching and an explicit ≤ 16 MB
  window and keeps whichever payload is smaller.  Two measured reasons it
  is a chooser and not a level bump: the window knee is sharp (body 29.8 MB
  column: 679 kB at the default window, 330 kB at 16 MB — the sentence
  pool repeats just past a small window), and high levels REGRESS
  small-alphabet streams (the id column: 54 kB at zstd-3, 143 kB at 19) —
  a global 19 would grow the ids while shrinking body.  Offline A/B and
  the container-walk dissection are recorded in
  `benchmarks/index-size/DESIGN.md` (new — it fixes the dangling link the
  #1039 README and this epic's anchor list pointed at).  Compatibility:
  the `ZBS4` magic is written only when a column actually uses codec 5/6
  or a form-1 RAW payload; anything expressible as ZBS2 stays
  byte-identical ZBS2 (flush parity and downgrade blast radius unchanged,
  `XERJ_FLUSH_PARITY=1` verified on a full 100k harness run), ZBS3
  present-null wrapping composes, the estimator counts COPY_OF as its
  source's bound and fails closed on crafted forward references, and the
  full decoder's fixpoint resolves reference chains while hard-erroring on
  cycles.  Measured on the 100k `benchmarks/index-size` harness at
  LEVEL=balanced against a same-day main control (`results/result-zbs4*-balanced.json`):
  `.seg` **−73.1 %** (1,671,271 → 449,094 B), total durable **−22.6 %**
  (5,398,478 → 4,176,300 B); every non-`.seg` extension byte-identical
  but for a 1 B `.json` manifest delta (47,366 → 47,365)
  between the runs.  ES-YAML conformance 1380 passed / 0 failed; 10 new
  storage unit tests cover typed-int round-trips (nulls, negatives,
  i64-extreme clusters), copy-of decode across full/projection/hydration
  paths, form-1 decode on all four read paths, and the malformed-payload
  rejections. (PR [#1042](https://github.com/xerj-org/xerj/pull/1042).)
- **`.post` block framing slimmed (ZPS2)** ([#1038](https://github.com/xerj-org/xerj/issues/1038)):
  the packed doc-id-delta and term-freq streams in every 128-doc posting
  block switch to frame-of-reference coding — `[width][vbyte min]` with the
  payload length derived instead of stored, width 0 meaning a constant block
  that carries no payload at all — and the positions block loses its u32
  length prefix (exactly 128 count+delta groups pin its end).  A positioned
  term whose `ttf == df` omits its ENTIRE freq stream; the reader re-derives
  that predicate from the `.meta` record it already holds, so the decision
  costs no bytes.  An all-equal block now costs 2–3 bytes where the ZPS1
  framing paid 21 (1 + 4 header bytes plus a payload of forced-minimum
  width 1).  The frame is subtracted only where it pays: when it narrows
  the width, or on narrow (≤ 4-bit) blocks whose level-shifted repeats
  normalise into identical payloads for the outer zstd pass; wide
  equal-width blocks keep raw lanes, because rewriting them measurably
  inflated the compressed `top_doc` field 12.7 % under an always-FOR
  encoder while its uncompressed stream shrank.  Same shape as tantivy's
  sorted blocks (vint base + packed residuals) and Lucene 50's per-term
  freq elision; adapted, not copied.  Segments written before the bump stay
  readable — the envelope magic (ZPS2 vs ZPS1 vs ZPL1 vs raw) selects the
  inner framing, and old formats keep their decoders.  Measured on the 100k
  `benchmarks/index-size` harness at LEVEL=balanced against a same-day base
  control: total −1.5 % (5,517,127 → 5,432,531 B), `.post` −3.4 %
  (2,449,405 → 2,364,905 B), `body.post` −4.3 % (1,828,102 → 1,749,695 B);
  the widest keyword family (`top_doc`, 12 terms × df ≈ 8.3 k) moved +1.5 % (PR
  [#1041](https://github.com/xerj-org/xerj/pull/1041)).
- **`.dv` numeric columns get a bit-packed codec (`ZNV2`), chosen per
  column by measured size** — stage 1 of the index-size epic
  [#1038](https://github.com/xerj-org/xerj/issues/1038)). The column is
  mapped through the monotone sign-flip bijection, given a global
  frame-of-reference (`u_min`) and a GCD divide, then stored as 128-doc
  blocks of bit-packed `u32` residuals — the shape tantivy's columnar
  uses (min-subtract + GCD-divide + bitpack; citations in
  `benchmarks/index-size/DESIGN.md`). Constant blocks (width 0) carry no
  payload at all, two-valued columns such as booleans collapse to 1-bit
  lanes after the GCD divide, block payload length is derived from the
  width byte (no per-block byte_len to store), and blocks whose
  post-frame spread exceeds 32 bits (mixed-sign f64 bit patterns) fall
  back to raw `u64` lanes. The writer compresses both candidates — `ZNV2`
  and the raw `ZNV1` layout — and keeps whichever is smaller after zstd
  at the actual level (the Parquet pick-the-smallest rule): the first
  cut shipped bitpack-always and the size harness caught it at 2.0× the
  `.dv` size on the 100k corpus, because that corpus *cycles* its 4k
  events and the outer zstd pass matches the repeating raw bytes almost
  for free, while block-local lanes destroy the byte-aligned repetition.
  With the chooser, the same 100k run measures `.dv` **−9.0 %** (355,156
  vs 390,487 B) and total durable **−0.9 %** (5,483,068 vs 5,532,276 B)
  on that worst-case-for-bitpacking corpus; monotone strided columns
  measure ~5× smaller than raw pre-compression in the unit tests, and
  real timestamp/counter columns have that shape. Null slots are
  excluded from the frame/GCD and re-zeroed at decode, so the decoded
  array is exactly what `from_iter` builds and the f64-ordered `sorted`
  index is rebuilt unchanged. `ZNV1` and the pre-magic legacy layout
  still decode via magic dispatch, so indexes written by older builds
  remain readable. (PR [#1040](https://github.com/xerj-org/xerj/pull/1040).)
- **The request-cache seen-set no longer allocates on the first tracked
  search** ([#1024](https://github.com/xerj-org/xerj/issues/1024)): a lazy
  seen-set took idle per-index RSS from 206 to 64 kB — ~3× margin under the
  0.2 MB/index idle budget line. (PRs
  [#1025](https://github.com/xerj-org/xerj/pull/1025) and
  [#1034](https://github.com/xerj-org/xerj/pull/1034).)

### Documentation

- The README says how XERJ works with Jev, in plain words (PR
  [#1010](https://github.com/xerj-org/xerj/pull/1010)).
- A field report from a local `/_decide` email-classification prototype
  session (PR [#1027](https://github.com/xerj-org/xerj/pull/1027)).
- `llms.txt` status claims caught up with `main`: rerank/share/mbox are on
  `main`, #948 is closed by #1002 with the new memory numbers, and the residual
  per-segment cache retention is re-anchored to #1032
  ([#1028](https://github.com/xerj-org/xerj/issues/1028)). (PR
  [#1033](https://github.com/xerj-org/xerj/pull/1033).)
- **The CHANGELOG's rc.19–rc.70 gap is closed (ROADMAP GA gate item).** The 52
  releases of 2026-08-19 → 2026-08-26 are reconstructed in
  [CHANGELOG.md](./CHANGELOG.md) from per-release manifests of `git log` (merge
  subjects and commit bodies) plus each tag's GitHub release record — 143
  entries carrying 281 issue/PR links, every link checked to sit inside its
  release window, 52/52 sections verified against their manifest (51 first
  pass, 1 rewritten). They are honestly drier than ship-time entries — numbers
  quoted only where a commit body recorded them — under a "Reconstructed
  2026-09-26" banner, and nothing was force-fitted: the three entries still
  unreleased at rc.71 stay under rc.72's "Recorded late". The same PR wrote
  the post-rc.77 window into [Unreleased] above and made three audit
  corrections, notably that the #874 idle gate is calibrated to 256 kB/idx,
  not the 0.2 MB line (that is #1024). (PR
  [#1035](https://github.com/xerj-org/xerj/pull/1035).)
- **`ROADMAP.md` rolled forward to a 2026-09-26 review — the stale-evening
  corrections, plus the rc.78 queue.** The 2026-09-21 review of the file was
  stale within hours: #950, #941 and #1015 closed that same evening, #874 the
  next day, and the zero-token stage-1 gating trio #937–#939 closed the same
  evening — every one recorded in the file as open or "under way". This pass
  re-checks every status claim against live tracker state: the *Next release*
  section now records what actually landed since the rc.77 tag (#1009, #1017,
  #1018, #1020, #1021, #1023, #1025, #1026, #1033, #1034) with PR links —
  including #874's idle budget met at ~3× margin (206 → 64 kB per idle index)
  and discussion #1012's email-labelling measurement (1.000 on the templated
  tier, 0.625 at 0.902 mean confidence on the hard one) — the open-defects
  shortlist shrinks to #1031 and #1032, the CHANGELOG-gap GA item is marked
  closed 2026-09-26 (the rc.19–rc.70 backfill, PR
  [#1035](https://github.com/xerj-org/xerj/pull/1035)), the stage-2
  object-storage bullet stops contradicting the file's own *Shipping today*
  section (#965 wired in rc.77), and the mail-ingest line is corrected to
  "shipped in rc.75", carrying the post-#1002 memory numbers (variant-C
  retention ~677 → ~108 MB) with #1032 named as the residual. The review line
  itself is scoped honestly as a desk review of post-rc.77 `main` and the
  live tracker — not a live re-verification — and the milestones claim is
  true again (rc.78 milestone created, all four open issues triaged). (PR
  [#1036](https://github.com/xerj-org/xerj/pull/1036).)
- **Every other public surface caught up too — the 2026-09-26 status sweep.**
  A 70-agent post-merge audit confirmed 58 stale-status findings beyond the
  `llms.txt` set #1033 fixed; this sweep corrects them all: `llms-full.txt`'s
  NOT-IMPLEMENTED block (object storage removed from it, remaining rows
  re-stamped against the code), the "no email handler" blanket claims across
  the answers/compare pages (mbox/`.eml` shipped rc.75), `RERANK.md`'s
  falsified "`min_score` means the same thing on every query" (replaced with
  the measured ECE 0.10–0.31 calibration and rank-don't-threshold guidance),
  `ZERO_TOKEN_DIRECTION`'s autoindex-resilience row (fixed in rc.75, #934),
  `XERJ_VS_LUCENE`'s `_update_by_query` fetch cap (post-#1023 `_id`-keyset
  paging), `SCRIPTING.md`'s code anchors (re-pinned after the `index.rs`
  refactors), the mail-recipe/takeout memory numbers (reframed as dated
  before-#1002 state), and the S3-alerting-plugins answer page that
  `llms.txt:50` cites as proof of the rc.77 S3 status while still denying it
  — rolled to the rc.77 reality with the correction stated in the open.
  Landing twins, both hubs and the sitemap regenerated via
  `build_articles.py --write`. Verified per-group adversarially and by a final
  gate (constants guard, `--check` for articles and sitemap, stale-phrase
  greps clean). (PR [#1037](https://github.com/xerj-org/xerj/pull/1037).)
- **A stage-1 index-size write-up on the blog: "Cutting the index 28%".** The
  post covers epic
  [#1038](https://github.com/xerj-org/xerj/issues/1038) stage 1 (ZNV2
  [#1040](https://github.com/xerj-org/xerj/pull/1040), ZPS2
  [#1041](https://github.com/xerj-org/xerj/pull/1041), ZBS4
  [#1042](https://github.com/xerj-org/xerj/pull/1042), ZFM5
  [#1043](https://github.com/xerj-org/xerj/pull/1043)): the dissect-first
  method, the four per-family A/B tables, the raw-vs-index baselines
  (63,461,256 B raw → 3,989,254 B durable, with the repetitive-corpus caveat
  stated), the final composition, and a losing-rows table (bitpack-always
  2.02×, the window knee 679 → 330 kB, ids 54 → 143 kB at level 19,
  `top_doc` +1.5 %, the stride-kept `.meta` at 138,727 B). The headline is
  anchored to the committed same-day pair 5,398,478 → 3,989,254 B (−26.1 %),
  with the −27.7 % reconstruction as the rounding behind "28%", and
  `doc_id.meta` after ZFM5 is the on-disk 216 B, not the 193 B offline
  estimate. The pre-publish verification pass also corrected two CHANGELOG
  numbers in place: ZBS4's total is −22.6 % from its own printed pair, and
  the byte-identical claim gained its 1 B `.json` exception. (PR
  [#1044](https://github.com/xerj-org/xerj/pull/1044).)
- **The xerj.org corner brand says XERJ, not XERJ.AI — and every blog post now ends on a
  conclusion plus a way to try the engine.** The top-left `<a class="brand">` nav link had been
  rendering the domain instead of the product name; it now reads "XERJ" on 179 hand-written and
  generated pages, in the print-header stamp on the 44 docs pages, in the dashboards corner span
  (`landing/playground/src/ux/chrome.js` and the committed `landing/playground/app.bundle.js`,
  edited identically — no node in this sandbox to rebuild), and in the
  `scripts/seo/build_articles.py:676` template, so regeneration cannot reintroduce the domain.
  Footers, tab titles, `og:site_name`/JSON-LD `alternateName` and brand-book specimens
  deliberately still read XERJ.AI — the scope was the corner, not a de-domain sweep. The blog
  endings review (user ask 2026-09-27) gave *does-xerj-beat-jev* a "05 · THE ANSWER" section and
  *jev-vs-a-bm25-vote* an "08 · THE TAKEAWAY", each scoped where the posts' evidence is (the
  ±0.01 tie rule to hosted-judge comparisons, the label-count crossover to Banking77, the
  calibration superlative to zero-shot arms; the superseded links-dump removed), and added a
  try-it paragraph to the blog hub (one binary, quickstart, playground, your own corpus as the
  benchmark that matters); `dateModified`/UPDATED bylines bumped to 2026-09-27 on the two edited
  posts with published dates unchanged, and every figure in the new copy verified against the
  posts' own bodies. Gates build_articles, heads, links, og card, seo_lint (0 violations) and
  the constants guard all green; sitemap regenerated in the same push to dodge the lastmod
  trap. (PR [#1045](https://github.com/xerj-org/xerj/pull/1045).)

## [1.0.0-rc.77] - 2026-09-21

The stateless-index and reader-fairness release. Two headline changes:
`storage.backend = "s3"` starts for real (#965), and readers stop starving
under sustained ingest (#1013) — both with fail-before reproductions.

### Added

- **`storage.backend = "s3"` works (#965, PR #1008).** Before this commit the
  startup guard refused the setting outright, honestly: a flush uploaded one
  file per segment while a 25-field segment family is 104 files, `snapshot.json`
  never left local disk, and a fresh node pointed at the bucket saw zero
  segments. Now the whole segment family packs into ONE immutable object — a
  ZBM1 bundle whose footer is the existing ZCM1 flush-completion manifest
  extended with absolute offsets (trailer shape adapted from quickwit's
  split-footer bundle, Apache-2.0, approach only). One object per segment plus
  the catalogue PUT is ~18% of Cloudflare R2's free Class-A tier at a 30 s
  flush interval, where 104 PUTs would be ~899%. `snapshot.json` per index is
  the publication point: merges publish output bundles before retiring inputs,
  boot fetches the catalogue and adopts backfill, reads hydrate families on
  demand, per-index meta rides the same bucket. Config grows to 128 settings
  (`storage.s3_bucket`); an empty bucket name stays a hard error (XERJ never
  calls CreateBucket). Fail-before, verified live on the base commit:
  `an_s3_index_round_trips_to_a_fresh_node` panicked "the flush must publish
  the snapshot catalogue". Honest limits stay documented in
  `docs/OBJECT_STORAGE.md`: single-writer v1, the WAL stays local, in-memory
  packs with no multipart upload, crash-consistency pinned against the
  in-process simulation only.

### Fixed

- **`/v1/systemone` votes on the payload, never on the instruction prose.**
  A question's `instructions` used to be concatenated into the retrieval
  query, so a well-written instruction destroyed accuracy — measured on the
  SMS history (300 held-out, seed 7): 0.9867 empty vs 0.6700 criteria-rich, a
  32-point swing from wording alone (#1000). And an object `state`
  contributed only its `query` field, so `{"message": …}` voted on the
  instruction at the spam base rate: accuracy 0.1733 vs 0.9667 for the same
  message as a string (#1001). The vote text is now exactly what the question
  points at: its backtick references (instructions data fields first, then
  state paths — both client shapes keep working), or, when it references
  nothing, the state's own string content (a string whole, an object by all
  string leaves). A reference that resolves to structure rather than text —
  the rubric object `jev-reranker` ≥ 0.1.2 ships with its relevance preset —
  is acknowledged and skipped, not embedded and not fatal. References that
  resolve to nowhere and payloads with no text are 422s naming the question.
  Also fixed the `no_support` error body, which interpolated `k` and the
  question ids into each other's slots.
  Fail-before: five new `systemone_http` tests, all failing on the old code
  (`[0.0, 0.0, 0.4545, 0.0]` across four instruction wordings of one state);
  a sixth pins the client's rubric shape, a 422 before the skip rule. After:
  the 300 held-out messages score 0.9767 identically across all eight
  wording/shape arms, and the pip-client acceptance gate passes through both
  client entry points (neutral-query gap 0.8542) and under a deliberately
  spammy query (gap 0.6087, threshold 0.3) — client 0.1.2, unmodified.

- **Readers no longer starve under sustained ingest (#1013, PR #1014).**
  `CollectionPublication` is a seqlock; `search()` admitted a reader and
  validated the token at the **end** of the search — demanding a writer-free
  *interval* spanning the whole scan. The flush path holds its guard across
  the entire finalize, concurrent flushes are allowed, and every document
  write takes its own bracket, so under sustained ingest writer-free
  search-length intervals stop existing: search and `delete_by_query` retried
  to their deadline and returned HTTP 500 (reproduced twice in the #950
  profiling run). Validation now happens at the **capture boundary**: a
  generation reference before the first collection-state read, validated with
  the seqlock evenness check right after the memtable snapshot — readers need
  a writer-free *instant*, not a writer-free search. A straddled capture
  returns a typed retry root (`CollectionCaptureCrossedPublication`,
  downcastable through `anyhow` context wraps) and `search()` retries until
  its deadline. Fail-before (`search_completes_under_publication_churn`): a
  churn thread holds a cancelled 1 ms publication bracket every 3 ms — old
  code FAILED at its 2 s deadline with exactly the production error; new code
  passes in 0.37 s, total exactly 5050. Gates: 723/723 engine lib tests,
  ES-YAML 1376/0 failed. Follow-up #1015 tracks shrinking the flush bracket
  itself (a latency consideration now, not a starvation hazard).

- **The console tour's pill assertions raced the route hand-off (#1011).**
  The browser security suite's policy tour waited on
  `aria-busy === 'false'`, but app.js only ever sets aria-busy to 'false' at
  the END of a render — nothing flips it 'true' while the next route's query
  is pending — so the wait was satisfied by the previous route's still-mounted
  DOM, and the fixed 150 ms sleep then read the old view's pill. Failed only
  on slow runners (PR #1008's CI): the window scales with runner latency. The
  tour now waits for the route's own scene, and a latency-injection
  demonstration failed the old assertion deterministically at 400 ms.

- **CI: the reference-coding toolkit's tests never ran.** `cargo test
  --features x -- xc_ y` passes one filter per invocation, so `xc_` matched
  nothing and the step was green on zero tests (3aba5786). And the toolkit
  job's MCP schema gate looked for a release binary that job never builds
  (8f8e5444). Both fixed; the job now genuinely executes.

- **docs: roadmap re-reviewed against rc.76** (release-drift guard, 213ce8f8).

## [1.0.0-rc.76] - 2026-09-21

The JEV-interface and honest-fixes release. The headline is a measured answer
to a specific judge: the node now speaks the Jev reranker's wire protocol
locally. Everything else in this cut is a fix with a reproduction behind it.

### Added

- **The node speaks System One — `/v1/systemone` + `/_decide`.** A rank-weighted
  kNN vote over a labelled-history index; nothing leaves the node. Two
  deliberate, test-pinned breaks from the hosted protocol: `model` echoes
  `xerj-history-vote-1` (never a Jev name), and zero support is a 422, never a
  fabricated 0.5. Acceptance gate: the unmodified pip-installed `jev-reranker`
  ran against the node and passed its own strict validation (spam 0.9014 vs
  ham 0.0956). `/_decide` on the ES-compat port returns the evidence —
  neighbours, labels, engine scores, weights. (#975)

- **Two public datasets on the record.** FiQA (BEIR, 57,638 docs / 648 judged
  queries; harness validated 0.2382 vs published 0.236): **BM25 0.2382 → Jev
  0.3638**, the judge's biggest lift and its worst calibration case (ECE
  0.3109). AG News (120k train history, all 7,600 test items): **0.9182
  accuracy, no model, no tokens**. Hosted-reranker rows stay labelled "not run
  by us"; no Jev row exists for AG News and none is claimed.
- **WAND fast path in xerj-fts — 2.0× on the vote, bit-identical.**
  `wand_should_bool`: doc-at-a-time top-cap evaluation for the shape every
  `match` query lowers to, strict-below pruning, allocation-free inner loop.
  AG News vote 68.7 → 34.2 ms/item; `/v1/systemone` p50 70.6 → 42.7 ms.
  Bit-identity proven three ways: BEIR figures reproduce to four decimals, a
  648-query A/B over a byte-identical index found 0 order and 0 score-bit
  changes, ES-YAML conformance 0 failed on the final binary.

- **`xerj code` and `xerj corpus add|index|list` — the reference-coding loop
  is in the binary ([#977](https://github.com/xerj-org/xerj/issues/977)).**
  `xerj code <corpus> "<query>"` retrieves the matching definition from a
  locally indexed corpus of peer projects (`-k`, `--lang`, `--mode
  bm25|semantic|hybrid`, `--full`, `--no-symbol`, `--meatl`, `--json`,
  `--stale-ok`, `--url`, `--api-key`), keeping the wrapper scripts' exit
  triangle: `0` hits, `1` no-match (including `--json` with empty hits),
  `2` usage/transport/30-day-staleness refusal, `3` corpus-in-ledger-but-not-
  loaded-on-this-node. `xerj corpus add` clones (or rebuilds from a pinned
  manifest via `--from`, preserving `review` blocks), detects licences with
  the restrictive-first heuristics and warns on approach-only sources;
  `xerj corpus index` is the #930 build-verify-swap (unverified builds are
  removed by exact name, an uncountable old index is never presumed empty,
  state switches by atomic rename only after `_count > 0`); `xerj corpus
  list` shows what is actually loaded on the node. Hybrid retrieval now fuses
  server-side — one native top-level `hybrid` query, RRF `k=60`, aimed at the
  semantic-capable indices, with an arms-ran note on every run. Existing
  `~/.xerj-code` corpora, state files and indexes work unchanged: same layout,
  same `corpus.json` schema, same `xc-<corpus>` namespaces. The `xerj_code_search`
  MCP tool (the 11th) exposes the same pipeline to shell-less agents, with
  `licence_policy: "strict"` stripping passage text from restricted-licence
  hits. No `python3`/`curl` dependency remains; `git` still is one (for
  `xerj corpus add`). Semantics live in the new `xerj-common::xccode` pure
  core, shared byte-for-byte by the CLI prose and the MCP tool text.

### Fixed

- **The rerank stage, fixed and measured honestly.** As it shipped in rc.75
  the `rerank` stage scored 0.3822 nDCG@10 where BM25 alone scored 0.7750
  (40-query SciFact pilot) — every candidate sat in one shared `state` blob, so
  the judge scored the pile once. Each candidate now rides in its own noul:
  **0.8299** through the real stage path. Full runs, 3 shuffled repeats each:
  SciFact **0.6572 → 0.7410**, NFCorpus **0.3016 → 0.3312** (still losing to
  the local hybrid 0.3448 — kept, not hidden). Two published caveats: provider
  non-determinism is ±0.01 nDCG@10, and the probabilities order well but are
  not calibrated (ECE 0.1023 / 0.1332).

- **Ingest memory: a 1 GB mailbox no longer aborts under the default 16 GiB
  cap, and at-rest settled memory halved**
  ([#948](https://github.com/xerj-org/xerj/issues/948), PR #1002). A 1 GB
  mailbox peaked at 25,356.9 MiB (24.8 GiB) VmHWM under the 16 GiB cap
  (aborted: 510/2,530
  needles missing, 0 edges; 68.5 GiB uncapped). The ingest-memory ledger found
  four causes: `memtable_shards` retained one parsed `Arc<Value>` per
  explicit-id write forever (flush drained the FTS memtable, never the storage
  shards — ~2.4-3.1x source bytes invisible to every ledger category);
  raw-bytes entries charged a flat 800 B regardless of payload; merge batches
  were cut by segment count while the executor materialises 5-9x input bytes;
  and the RSS breaker only 429'd, never drained. Now: published memtable
  entries are pruned at flush finalise, entry charges are structural, merge
  batches are cut by input bytes (`XERJ_MERGE_BATCH_MAX_INPUT_MB`), and an
  engaged breaker drains (flush + release caches + jemalloc arena purge,
  rate-limited). Measured, same harness both sides: at-rest settled
  972.8 -> 481-488 MiB (variant A, 3 runs) and 883.0 -> 280.8 MiB (variant C,
  100k x 100 B; retention ~677 -> ~108 MB). Merge batching follows the
  quickwit/tantivy approach (streaming_writer.rs:378, log_merge_policy.rs:22 —
  no code copied). Residual, separately tracked: three per-segment caches
  (`stored_value_cache`, `dv_cache`, `id_pos_cache`) remain unbounded until a
  merge retires their segment —
  [#1032](https://github.com/xerj-org/xerj/issues/1032).
- **scalar8 keeps its promise: 2.3x faster kNN, and `_score` no longer depends
  on the filter**
  ([#392](https://github.com/xerj-org/xerj/issues/392), PR #999). scalar8
  advertised a ~4x smaller vector working set and an ES-like score and
  delivered neither: candidates were quantised per query from `_source`
  f32s (nothing smaller resident), and the SQ8 codebook was fitted per query
  over the candidate set, so `_score` changed with the filter (measured:
  up to 2e-05 drift, 19/30 order swaps from an unrelated-only filter). Codes
  are now written at ingest into a per-field flat `Sq8CodeStore`
  (slot-addressed, tombstoned on delete, codebook fitted from ingested
  vectors and widened on out-of-range), re-derived by a background
  authoritative walk on restart, and served by `run_knn_sq8_codes_scan` —
  which fails safe to the exact scan on any coverage gate miss. Measured,
  50k x 128-d cosine: recall@10 0.9700 -> 0.9620, p50 0.55 -> 0.24 ms
  (**2.3x**), codes resident exactly 50k*128 bytes, filter drift **exactly
  0.0**. Scoring working set 4x smaller; total memory is not (f32 `_source`
  kept) — docs say both. qdrant's encoded_vectors_u8.rs approach adapted
  (Apache-2.0, cited in module docs); Lucene approach-only.
- **The Windows build compiles again: the jemalloc purge hook is cfg-gated out
  on MSVC** (PR #999). #948's boot hook called `tikv_jemalloc_ctl`
  unguarded, but the tikv crates are target-gated out on msvc in
  xerj-server/Cargo.toml — every merge-ref off main failed the Windows FD
  smoke with E0433 since #948 landed (the serialised runner queue completed
  the job after the merge decision, which is why main slid red for two
  merges). Statement-level `#[cfg(not(target_env = "msvc"))]` on the hook
  install only; the engine's default no-op purge covers MSVC and the
  resource sampler still runs everywhere.
- `_bulk` no longer truncates an action-line `_id` at an escaped quote: two
  ids sharing a prefix up to a `"` collapsed onto one stored id and the
  second document silently overwrote the first; `\\`, `\/` and `\uXXXX`
  in ids were stored as literal escape text
  ([#954](https://github.com/xerj-org/xerj/issues/954), PR #983).
- The docs fact-check rule no longer rejects `xerj search`, which exists: the
  pattern's subcommand list predated the query clients, so every page
  demonstrating the search CLI shipped with an ERROR-level finding
  ([#963](https://github.com/xerj-org/xerj/issues/963), PR #978).

- `mcp`: `xerj_search` defaults its index to the `ax-*` autoindex prefix, so
  the documented no-argument call works on a fresh node (#962, PR #980).
- `xerj feedback --open-pr` only runs in a checkout of xerj-org/xerj — it no
  longer commits and pushes into whatever repository it is run from (#960,
  PR #981).
- Console passkey enrolment works on any port: the WebAuthn RP is derived from
  the bound address instead of a hard-coded localhost (#935, PR #987).
- Filters around a hybrid query are applied instead of ignored: a
  `bool{must: hybrid, filter}` pushes the filter into every leg, and
  `post_filter` beside `hybrid` is rejected with a 400 (#943, PR #988).
- `filter` / `filters` aggregations with wildcards or regexps share the query
  path's matchers instead of counting 0 or everything (#959, PR #992).
- `autoindex --no-graph` replays the index phase through a bounded parallel
  window — **1.64× the index phase** on the repro corpus, deterministic by
  construction (#933, PR #989).
- `xerj init` on a default (auth-on) node writes an MCP entry that actually
  works: the loopback console falls back to the admin key instead of answering
  401 to every tool call (#961, PR #982).
- A `fields` clause is no longer silently dropped when `_source` includes omit
  the field — both apply, as siblings (#932, PR #991).
- A declared per-field analyzer keeps applying after `_flush`: analysis config
  persists instead of reverting to the memtable-only default (#937, PR #994).
- Neural ingest no longer keeps ~3.4 of 32 threads busy behind one window:
  bounded window concurrency for a single `_bulk` stream (settings count now
  127, enforced by `journey_zero_config`) — first slice of #938 (#938, PR #995).
- `autoindex`: a one-file change no longer costs O(corpus) twice — snapshot and
  catalog updates are incremental (#971, PR #998).
- Semantic/hybrid over multi-passage documents hydrates only the winners
  instead of deep-cloning every `_source` per query — the repro query drops
  ~410 ms (≈2.3×) (#939, PR #979).
- Generated articles carry an explicit `updated:` date so regenerated pages
  stop drifting on merge (#974, PR #984).
- Console graph (Reader panel, Second Brain dashboard) reads go through the
  signed-in console session instead of keyless routes — a signed-in operator on
  an auth-on node gets the graph, and a refused listing says so instead of
  rendering as an empty graph (#936, PR #996).

- **The Second Brain dashboard and the reader's graph panel are no longer 401
  for a signed-in operator on an auth-enabled engine (the default).** The
  console holds a passkey session, not an engine API key, so the SPA's direct
  `/_graph/{brain}/*`, `_cat/indices/.xerj-memory-*` and reserved-namespace
  `_search` reads were refused — while a share-link guest, who holds a key,
  could read the graph. The console now serves its own brain-scoped,
  session-authorized graph read path
  (`GET /_xerj-console/api/v1/graph/brains`, `/{brain}/ego`, `/{brain}/overview`,
  `POST /{brain}/edges/_search`, `POST /{brain}/nodes/_search`) in the same
  `xerj-second-brain/1` contract as the data plane: console roles `owner` and
  `admin` may read brains, a role-refused read and a nonexistent brain are
  byte-identical 404s (no existence oracle), and the nodes/edges searches are
  pinned server-side to the brain's own meta-doc indices, so the endpoints
  cannot become a general search proxy and the generic data-sources proxy
  still refuses the reserved namespace. `/v1/metrics` (the dashboard's
  searches-per-index tile) stays key-only by decision; the tile shows the
  refusal honestly. [#936](https://github.com/xerj-org/xerj/issues/936)

### Packaging

- .deb assets ship with every release (amd64/arm64), built by
  `scripts/build-deb.sh` and verified by `scripts/verify-release.sh`;
  docs/PACKAGING_DEBIAN.md states the Debian archive path honestly (#810).

### Removed

- `tools/xerj-code/scripts/{xc.py,xc-index.sh,xc-corpus.sh}` and their
  Python/shell test suites. Most behaviour is ported and gated by scoped cargo
  tests (`xerj-common::xccode`, `xerj-autoindex::xc`, the MCP schema drift
  test); the `--fresh` swap contract — the destructive side of
  `xerj corpus index --fresh` — is re-pinned in `xerj-autoindex/src/xc.rs` as
  ten contract tests running the REAL flow against a fake node and a fake
  autoindex runner behind injected seams
  ([#1004](https://github.com/xerj-org/xerj/issues/1004)): switch-readers-
  before-retiring, deletes by exact name only, a failed or empty build leaving
  the old index untouched, a count the node never answered authorising no
  delete and no switch, sibling corpora never touched, first-build salvage,
  resume under the recorded prefix and state dir, build-id collisions inside
  one second, and the legacy no-ledger path. The deliberate
  drops are the post-query field-report nudge (2026-09-18 llms.txt directive:
  no obligation language) and per-hit hybrid arm annotations (server-side RRF
  exposes no per-leg ranks).

## [1.0.0-rc.75] - 2026-09-20

### Added

- **`xerj share <index|folder>` — give one person read-only search over one
  indexed folder: a link, a passcode, an expiry.** The guest opens the link,
  types the passcode and gets a reading room (search, highlighted snippets, a
  document view) served by the owner's node; the folder is not uploaded and
  the guest installs nothing. `POST /_share` (admin key only) stores only a
  SHA-256 digest of the share id and an Argon2id hash of the passcode; the
  unauthenticated, rate-limited, audited `POST /_share/claim` takes
  `{id, passcode}` in its body, so the share id is not in the request line an
  access log records, and mints a scoped read-only key that expires with the
  share; a guest-only route allow-list closes `_cat`, `_cluster`, every other
  index, all writes, and scroll / PIT / async contexts. Every response under
  `/_share` and every response to a guest key is `no-store`; a guest's refused
  attempts to manage shares or mint a key are audited; the `autoindex-catalog`
  index cannot be shared. `--tunnel` supervises your own `cloudflared` quick
  tunnel and revokes the share on Ctrl-C. That traffic passes through
  Cloudflare, which terminates TLS and can read the passcode, the guest key and
  the documents; the command and the guest page both say so. `--list` /
  `--revoke <handle>` manage shares; the command refuses a node running with
  authentication off, where a read-only key would restrict nothing.
  `xerj brain` now prints the share command for the folder it indexed. Found
  and fixed on the way: the graph API
  authorized a multi-dataset brain's comma-joined `nodes_index` as one literal
  index name, so every scoped key got `403` on `overview` for any folder with
  more than one dataset (and `overview` reported 0 notes for it); and the
  Console asset bundle was not rebuilt when a new file appeared under
  `xerj-ux/` on a warm target directory. Threat model, guest reach table and
  the quick-tunnel trade-offs: [docs/SHARING.md](docs/SHARING.md).
- **`xerj autoindex` reads mbox mailboxes and Google Takeout exports** — an mbox
  (Takeout, Thunderbird, Apple Mail, mutt) is detected by content, split in a
  bounded-memory stream with `>From ` unquoting, CRLF/LF and a missing final
  newline handled, and every message goes through the same extractor as a
  standalone `.eml`, so message and attachment records (PDF pages included)
  have one shape; senders and recipients are filterable by bare address
  (`email_from_address`, `email_to_address`, `email_cc_address`), Subject
  words are searchable in `body`, and Gmail's decimal `X-GM-THRID` is stored
  as the hex id Gmail's web interface uses; messages are parsed on a
  `--workers`-wide pool under one process-wide in-flight byte budget and
  forwarded in order, the progress bar moves inside the file, a Takeout
  root's `archive_browser.html` and Keep `.html` twins are skipped by named
  rules, Keep `.json` notes are indexed, and an unextracted archive is named
  under the run's summary with the command to extract it. The new
  `email-thread@1` detector writes `replies_to` (`In-Reply-To`/`References`)
  and `attachment_of` edges with evidence, and an incremental run keeps a
  reply across two mailboxes (Inbox → Sent) by loading the untouched
  mailboxes' messages back from the index. A loading run now waits the
  server's memory circuit breaker out — a 429, per item or as the HTTP
  status, is re-offered (only the rejected records) for up to ten minutes
  instead of aborting the run. Numbers, machine and commands:
  `benchmarks/mbox-ingest/README.md` (synthetic mailbox; not yet verified on
  a real Takeout export). Measured limits, filed on
  [#948](https://github.com/xerj-org/xerj/issues/948): the client peaks under
  300 MiB on a 1 GB mailbox, but the node needs 66.9 GiB of RSS to finish it
  and does not finish under its default 16 GiB cap; at a 16 GiB laptop's
  8 GiB cap a 300 MB mailbox completes with the node peaking at 18–20 GiB;
  and after a restart, a first-time query on the 1 GB index takes either
  under 50 ms or seconds (up to 16.6 s measured) until the node warms.
- **`xerj autoindex <folder> --watch` keeps an index current from filesystem
  events instead of a re-run.** The session indexes once, then places one OS
  watch per *indexed* directory (`notify`: inotify / FSEvents /
  ReadDirectoryChangesW) and reindexes what changed, debounced (`--debounce`,
  default 400 ms) so one editor save is one pass. The watch set comes from the
  same walk the indexer uses, so an ignored `target/` costs no watch and
  produces no events, and a watched run and a re-run agree on what is indexed.
  A file skips its re-hash only when no event named it or an ancestor AND its
  `(size, mtime, inode)` fingerprint is unchanged; the cache is in-memory, so a
  restart re-hashes in full. Requires `--no-graph`, refused rather than
  downgraded: reconciling an ADDED or DELETED file exists only on that route —
  on the graph path a re-run skips a file added after the resume plan was frozen
  (exit 3, `appeared after the resume plan was frozen`) and ABORTS on a deleted
  one (exit 1, and every re-run after it), so a watcher there would go stale on
  the first new file and stop reindexing on the first deletion. A file whose
  CONTENT changed is reconciled on the graph path, measured at 3.07 s; the
  earlier claim that it is not came from a measurement whose shell append had
  created a file instead of modifying one. Measured on a 10,000-file /
  4,576,300-byte tree, single samples on a shared box: idle costs 0.00
  CPU-seconds per minute and 0 bytes read (holding 157 MiB and 295 threads),
  against 1.7 s wall / 2.1 CPU-s and a full corpus re-read for every poll of a
  re-run loop. Per change `--watch` is not faster than re-running the same
  command (47.6 s / 6.6 CPU-s against 44.3 s / 7.6 CPU-s for one modified file);
  both beat re-indexing the folder from scratch (293.8 s / 127.9 CPU-s) by an
  order of magnitude. The pass cost is two O(corpus) terms neither route avoids
  — a ~13 s snapshot over the whole inventory
  (`sync_executor::create_snapshot_inner`) and ~27 s of server CPU rewriting one
  catalog document per file — the measured next lever, not fixed here. Hitting
  `fs.inotify.max_user_watches` stops the run with the limit, the directory
  count and the `sysctl`, because a half-watched tree looks live and silently is
  not. Docs: `docs/LIVE_REINDEXING.md`, measurement record in
  `docs/measurements/autoindex-watch-2026-09-19.md`.
- **`xerj autoindex s3://bucket/prefix` indexes an S3-compatible bucket** —
  Amazon S3, Cloudflare R2 (`r2://`), MinIO, Ceph or anything else that speaks
  S3, via `--endpoint-url` (falling back to `AWS_ENDPOINT_URL_S3` /
  `AWS_ENDPOINT_URL`). The bucket is a *source*, not a second product: the
  prefix is listed with ListObjectsV2, each object is streamed into a local
  mirror under `--state-dir`, and the ordinary discovery pipeline — sniffing,
  the code and document extractors, the plan, the resume journal, the
  incremental reconcile — runs over that mirror unchanged. Credentials come
  only from the environment (`AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` /
  `AWS_SESSION_TOKEN`) — no profile files, no instance metadata, no SSO, and
  never from the URL (`s3://key:secret@…` is refused by name). `aws-config` is
  deliberately not a dependency: it would re-enable the SDK's default HTTPS
  client and with it the `aws-lc-rs` C/assembly crypto backend that this
  workspace keeps out of its cross-compile matrix, and it would bring a second
  S3 client with the SDK's own invisible retry layer. Change detection is the ETag plus the size, treated as an
  opaque token: a multipart `-N` ETag is stored and compared verbatim and never
  mistaken for an MD5, and a store that returns no ETag falls back to
  last-modified plus size with the count of such objects reported. Keys that
  cannot become a safe portable path (`..`, control characters, Windows
  reserved names, case collisions), dotfiles and the built-in build-output
  list are filtered out of the listing, so they never cost a request. Cost is
  printed by every run, in the two classes that are billed: a scan is
  `ceil(N/1000)` LIST (class A) plus one GET (class B) per changed object, and
  the run also prints what the same command would cost daily, hourly and every
  five minutes against a 1,000,000/month free allowance. Those counts are
  **billed wire attempts**: the SDK's own retry layer is disabled and the
  client charges its counter before each attempt, so a throttled request that
  succeeded on its third try reports three, and one that exhausts its retries
  reports what it spent instead of nothing. A run that fails mid-transfer
  records the objects whose bytes already landed (and checkpoints every ten
  seconds during a long one), so the re-run pays one GET per object it had not
  already fetched rather than paying for all of them twice; the LIST cost is
  paid again. Measured end to end
  against a live node and MinIO: first index of six keys (including a 12 MiB
  real multipart object) 1 LIST + 5 GET; unchanged re-run 1 LIST + **0 GET**;
  one changed object 1 LIST + 1 GET; one deleted object (`--no-graph`, the
  journal that reconciles deletions) removes exactly that object's documents.
  A 1 GiB object streams through for 4 MB of RSS growth (22 MB → 26 MB), in
  281–318 ms over four loopback runs — the memory figure is the claim; the
  milliseconds are a same-host transfer and not a throughput benchmark. Nothing is
  ever written to the bucket, and the index stays on the node's local disk —
  `docs/OBJECT_STORAGE.md` states both, with the request arithmetic and the
  measured runs.
- **A real S3-compatible object-storage backend — Cloudflare R2, MinIO and AWS
  S3 — with per-request cost accounting.** `xerj-storage`'s `S3Backend` was a
  local-directory simulation that its own doc comment admitted to; it is now an
  `aws-sdk-s3` client doing ranged `GetObject`, `PutObject`, paginated
  `ListObjectsV2` and `HeadObject`, verified against both MinIO and R2
  (including the 1,200-object `ListObjectsV2` page boundary, listed in exactly
  two Class A operations). The simulation is kept as `SimulatedObjectStore`, a
  test double, because unit tests want a backend with no network and no cost.
  Because object stores bill per request and Cloudflare R2's free tier allows
  1,000,000 Class A operations a month *per account* — about 23 a minute for
  everything — the backend counts every billed attempt by class and exposes it
  through `StorageBackend::ops()`, and `OpBudget` refuses to send more past a
  ceiling. Counted per wire attempt, which is why the AWS SDK's retry layer is
  disabled in favour of XERJ's own: an SDK-internal retry is invisible to a
  counter wrapped around the call. A listing is bounded by a seen-token check
  (which catches a continuation-token cycle of any length, not only an
  immediately repeated token) and by `S3Config::max_list_pages`, so a broken or
  hostile endpoint cannot bill a Class A request per page without end. `SegmentCache` gained hit/miss/bytes
  accounting, `get_range` (whole-object fetch on miss, then slice) and
  `get_range_uncached` (fetch the range only) — measured against MinIO on
  loopback at 5.98 ms cold, 1.67 ms range-only and 0.068 ms warm, and, in a
  single unrepeated run over a ~1 MB/s link that is not a performance claim
  about R2, against R2 at 3.72 s, 0.67 s and 0.12 ms. **`storage.backend =
  "s3"` still refuses to start**: nothing routes the index's segment reads and
  writes through the backend, the flush path that exists uploads 1 of a
  segment's 104 files, and `snapshot.json` never leaves local disk, so a fresh
  node pointed at a bucket sees zero segments — asserted by
  `object_store_mode_does_not_yet_make_an_index_stateless`, which also shows
  that a fresh node *can* fetch and read a segment from the bucket by id. Full
  arithmetic, measurements and remaining work in
  [docs/OBJECT_STORAGE.md](docs/OBJECT_STORAGE.md).

- **`xerj autoindex s3://bucket/prefix --watch` keeps a bucket-backed index
  current with a poll whose cost is bounded and visible.** An object store has
  no inotify, so change tracking is polling or event notifications. This lands
  polling, because `ListObjectsV2` is served by S3, R2, MinIO and Ceph RGW
  alike and because a poll is self-correcting: every cycle re-derives the truth
  from the bucket, so a dropped notification cannot leave an index permanently
  wrong. Each cycle lists the prefix, compares ETag, size and last-modified
  against a per-object journal, reads only what changed, and emits one JSON
  event per added, changed or deleted object — it produces a change feed and
  does NOT index, and it is not `xerj autoindex s3://bucket/prefix`, which
  indexes a bucket once; wiring the feed into that indexer plugs into the same
  `ChangeSink` and is still to come.
  The cost model is the feature, not a footnote: `ListObjectsV2` is a **Class A**
  operation at **at least** one call per 1,000 keys per cycle, charged whether
  anything changed or not (at least, because when a store stops paging is its
  own choice: MinIO serves 11 calls for 10,000 keys, not 10, so the projection
  takes whichever is larger, the arithmetic or what the store just served), against Cloudflare R2's free tier of 1,000,000 Class A
  operations a month (~23/minute for a whole account). A 5 s poll on an *empty*
  bucket costs 518,400/month — half the tier to watch nothing; a 60 s poll on a
  100,000-object prefix costs 4,320,000/month, over four times the whole
  allowance. So the default interval is 300 s, the default budget is 200,000
  Class A ops/month (20% of the tier, because the rest of the account spends
  from it too), and a cycle whose projection exceeds the budget is **refused**
  with exit 4 and a decision-request document naming the minimum safe interval
  — the same contract the folder-indexing gate uses. The budget is a CIRCUIT
  BREAKER on every cycle, not a greeting on the first: a prefix that grows past
  it mid-watch stops the watch, and so does a month whose allowance is spent
  (Class A and Class B are counted in `<state-dir>/objwatch-spend.json`, which
  survives restarts, so a supervisor restart loop cannot mint a fresh budget
  each time — the ledger is written BEFORE the calls it pays for, sixteen at a
  time, so a `kill -9` in the middle of a long first scan still leaves the run
  owing what it spent, and a ledger stamped with a future month is a backwards
  clock step rather than a new allowance). GETs have their own budget, `--max-monthly-gets`, default
  2,000,000 (20% of the 10,000,000 Class B tier).
  `--append-only` turns a growing key space into one list call per cycle via
  `start-after` (and cannot detect deletes, which is why it is opt-in),
  `--no-fetch` is metadata-only at zero Class B operations, `--dry-run` prices
  a poll and records nothing at all (no read, no event, no journal) though the
  listing it makes is real and billed like any other cycle, changed
  objects are fetched 8 at a time with the events still emitted in listing
  order, keys come back from the listing byte for byte (whitespace and entity
  references included), an edit past `--max-object-mb` is always emitted
  because a prefix digest cannot prove the bytes are unchanged, and one
  transient listing failure after the first cycle is retried at the next poll
  rather than ending the watch (five in a row end it), and every cycle reports list calls, reads, bytes
  and wall time to stderr plus a status file an operator can read without
  attaching to the process. Cycles never overlap: the schedule is fixed at
  `t0 + k*interval` and deadlines a long cycle passed through are counted and
  reported, because an overrun means the effective interval is longer than the
  one the budget was computed from. Event-driven delivery (S3 Event
  Notifications, R2 event notifications) is **not implemented**; what it would
  take, including the reconciling pass that at-least-once delivery makes
  mandatory, is written down in
  [`docs/WATCHING_OBJECT_STORAGE.md`](docs/WATCHING_OBJECT_STORAGE.md).

- **`hybrid: true` in `POST /_memory/{ns}/_recall` fuses BM25 and server-side
  semantic recall inside the memory API**
  ([#918](https://github.com/xerj-org/xerj/issues/918)). Recall used to pick
  exactly one mode — `vector`, `semantic: true`, or plain `query` — so an agent
  that wanted fused recall over a namespace had to bypass `/_memory` and issue
  a raw `hybrid` `_search` against the backing index, losing the memory hit
  shape, the `recency_weight` blend, the graph coupling and the namespace
  authorization path. `hybrid: true` now runs the BM25 leg and the semantic
  leg over the same `query` as the two legs of the engine's `hybrid` query
  (reciprocal rank fusion, `fusion: "linear"` opt-in), each leg keeping the
  filter shape it dispatches with and fetching the same over-fetch width the
  re-rankers already use. Off by default; `vector` or `semantic: true` beside
  it, `fusion` without it, and `fusion: "learned"` are 400s, so a request
  never silently degrades to one leg. The MCP `xerj_memory_recall` tool
  exposes the same two parameters. Raised by @Vinz2168 from a shared-memory
  agent integration where neither single mode was enough.
- **`rerank` stage on `_search`: hand the top hits to an external relevance
  judge and reorder by the calibrated probability that comes back.**
  `"rerank": {}` sends the question and the text of the top `window` hits
  (default 30, max 300) to TypeSafe AI's Jev; the 0–1 probability replaces
  `_score`, so `rerank.min_score` is an absolute cut-off, which a BM25 score
  cannot be. **It is the only search-time feature that sends document text
  off the node** (`[embedding] default_endpoint` and the WAL tap also send text
  off the node when an operator configures them; docs/RERANK.md lists every
  outbound connection a node can open, and a test fails when the engine source
  gains one that list does not name): inert until
  an operator sets `[rerank] api_key` (or
  `TYPESAFE_API_KEY`; config wins over env), opt-in per request, forbidden
  outright by `[rerank] enabled = false`, and only fields the response returns
  are sent — `rerank.fields` is an exhaustive allow-list. Failure policy is
  *degrade on deadline, surface on contract*: a slow provider is a 200 with the
  engine's order and `_rerank.applied: false`; no key is 503, disabled 403, a
  rejected key or malformed body 502, all with no hits; a provider that answers
  for nothing it was sent is a 200 with `applied: false`. When applied, every
  `_score` is a probability or `null` (a hit with no verdict, counted in
  `_rerank.unjudged`, sorts last) — never an engine score beside probabilities.
  `hits.total` and `aggs`
  stay the engine's; paging happens inside the window; `sort`, `search_after`,
  `collapse`, scroll and `size: 0` are 400s; and `_msearch`, search templates,
  `_async_search`, `_rank_eval` (per request, under `failures`), the native
  `/v1` search API and gRPC refuse the block instead of dropping it. Every
  caller-chosen cost knob has a server-side ceiling, the strings included:
  `instructions` is capped at 2,000 characters because the provider's wire
  format repeats it once per judged document, the question at 4,000 and
  `model` at 128; a provider response over 2 MiB is a 502, not an allocation.
  `GET /_xerj/rerank` reports whether a provider is configured
  and never the key; `/v1/metrics` gains `xerj_rerank_requests_total{outcome}`,
  `xerj_rerank_documents_judged_total` and
  `xerj_rerank_provider_tokens_total{kind}`; the MCP `xerj_search` and
  `xerj_hybrid_search` tools take an optional `rerank` argument. **Ranking
  quality with the real model is not verified** — no provider key was
  available, and every HTTP test runs against an in-process test double.
  New crate `xerj-rerank`; reference in `docs/RERANK.md`; two benchmarks,
  `benchmarks/beir-hybrid` and `benchmarks/decisions-as-retrieval`, both
  measured with `--embed-mode neural`, not the default lexical embedder. The
  one-question-per-document request shape follows `hev/jev-rerank`
  (Apache-2.0); the failure policy and the retry back-off constants follow
  Meilisearch's personalization module (MIT; adapted, cited in code).

### Fixed

- **`xerj autoindex`: one dataset the server refuses no longer aborts the run,
  `--no-graph` progress reports the phase it is in, and `xc-index.sh --fresh`
  works on a corpus that was indexed before**
  ([#929](https://github.com/xerj-org/xerj/issues/929),
  [#931](https://github.com/xerj-org/xerj/issues/931),
  [#930](https://github.com/xerj-org/xerj/issues/930)). On rc.74 a single HTTP
  400 on one dataset's mapping ended a 48,533-file run with `exit=1
  reason=aborted` and nothing indexed. A 400 on create-index / put-mapping is
  now a refusal of *that dataset*: its files are recorded as junk with the
  server's reason, every other dataset is indexed, the run exits 3, and
  `xerj-done`, the catalog run document and `xerj autoindex map` all carry
  `datasets_refused` / `files_refused`, so a generation that lacks a dataset
  cannot read as a whole one. 401/403/404/408/429/5xx still abort. On the
  `--no-graph` path the stream used to print `phase=scan pct=100.0
  eta_quality=stalled` for the whole time documents were landing (48 such lines
  in the rc.74 capture) — indistinguishable from a real hang; mapping install,
  sealing, indexing and the read-back barrier are now the `prepare`, `snapshot`,
  `index`, `finalize-catalog`, `finalize-refresh` and `finalize-verify` phases
  with real denominators. `tools/xerj-code/scripts/xc-index.sh --fresh` no
  longer forwards the flag to autoindex (which refuses it once a generation has
  committed): it builds a replacement beside the old index, verifies
  `_count > 0`, switches the state file atomically and only then retires the old
  indices by exact name; a record count the node does not answer is never read
  as zero, so it cannot get a working index retired or a finished build
  deleted. A first build that is interrupted after writing
  records is kept rather than leave no corpus, and `xc.py` now reads the
  `salvaged` / `autoindex_exit` the script always recorded: it warns on every
  query that coverage is INCOMPLETE, so a miss against a partial index cannot
  read as "this code does not exist". Found while verifying the above on the full corpus
  ([#944](https://github.com/xerj-org/xerj/issues/944)): a per-item HTTP 429
  inside one bulk — the node's memory circuit breaker, which engages and
  releases within seconds — aborted the 48,533-file run at 60% after 85
  minutes. The rejected items are now re-sent, and only those, after a backoff,
  until the node takes them or the bulk's 600 s of patience runs out — a
  429 on the whole bulk request is the same back-pressure, not six transport
  attempts (that split ended the resumed run);
  only then is it exit 1, resumable, with an error line that says so. The
  terminal line carries `bulk_retries=N` when it happened, and the
  `raising bulk concurrency` line is printed at most once per 10 s (117 lines
  for 11 shrinks in the capture). Captures: `benchmarks/autoindex-resilience/`.

- **`xerj autoindex`: a corpus whose catalog holds more documents than the
  server takes in one request no longer fails at the very end**
  ([#955](https://github.com/xerj-org/xerj/issues/955)). Both indexing paths
  sent the catalog (one document per file, per dataset, per run) as ONE
  `_bulk`. On the 48,533-file reference corpus that was 51,129 actions in
  31.9 MB against the engine's default `limits.max_actions_per_bulk` of 50,000:
  the `--no-graph` run applied all 47,444 operations, then ended
  `exit=1 reason=aborted` in `finalize-catalog` after 10,336 s. On the default
  path the same answer was silent: exit 0 with an empty catalog (reproduced by
  a test against a stub that returns the engine's literal 413). Every `_bulk`
  body now goes out in windows of at most 10,000 actions, and the catalog also
  under `--bulk-mb`. A request the server still refuses as too large (HTTP 413,
  or one item answered 413 for a body of several actions) is halved and
  re-sent, and the bound is kept for the rest of the run; the terminal line
  carries `bulk_splits=N` when that happened. Resuming that same generation
  with the fix committed it (`ok=true exit=3 … records=821840`, 51,129 catalog
  documents). A `--no-graph` run that runs out of back-pressure patience now
  ends `reason=server-backpressure` with `ops_applied` / `ops_remaining`
  instead of `reason=aborted`; it is still exit 1 and resumable.

- **Reference-code passage windows preserve their match score.** `xc.py`
  could select relevant source and then discard its matches while aligning
  the excerpt to line boundaries, including on long source lines. Line
  alignment now keeps the selected term score or falls back to the bounded
  original window. Window scoring also uses original-source offsets when
  Unicode lowercasing expands characters. This affects the fallback when no
  matching symbol is available and explicit `--no-symbol` output.

- **`xc.py --mode hybrid` keeps BM25 results when the optional vector arm has
  a transport failure.** Connection failures, read timeouts, and interrupted
  HTTP responses during semantic mapping discovery or search now take the
  existing BM25-only fallback instead of aborting and discarding valid hits.
  Primary BM25 failures still exit with an error. Standalone `--mode semantic`
  now reports mapping/search HTTP and transport failures as errors (exit `2`)
  instead of treating failed requests as empty search results (exit `1`).

- **`xc.py --json` emits JSON for empty search results.** Previously, a query
  with no hits printed the human-readable no-match message before reaching the
  JSON output branch, breaking callers that parse stdout. Empty results now
  preserve the JSON response and still exit `1`; matching results exit `0`, and
  a corpus with no live indices retains its distinct exit `3` diagnostic.

### Documentation

- **`llms.txt` proposals, revised and fact-checked**
  (`docs/research/llms-txt-2026-09/proposals/`). A proposed `llms.txt`, an
  `llms-install.md` and ten paste-ready install prompts, following the study's
  twelve rules: three complete entry paths (shell, MCP-only, HTTP-only), the
  lexical-by-default correction first, per-client MCP registration with the
  key, a verify line after every step, and a feedback ask that is optional
  and needs no git. Every quote is re-fetched (259 claims, 258 confirmed, the
  one failure removed) and the XERJ commands the proposals print were run on
  Linux against v1.0.0-rc.74, except the ones the record's own "Not run" list
  names — the `curl | sh` installer, macOS and Windows, `--embed-mode neural`,
  `claude mcp add --scope local` and the per-client registration lines, which
  are quoted from each client's own documentation with a fact-check id. Two product findings came out of it:
  `xerj feedback --open-pr` branches, commits and pushes in whatever
  repository it is run from, and `xerj init` writes an MCP entry without
  `XERJ_AUTH`, so against a default node every tool call returns 401. The
  live `landing/llms.txt` is unchanged; the report's ship checklist says what
  has to exist first.

- **ROADMAP: the zero-token direction, with every status checked against the
  tree** ([#941](https://github.com/xerj-org/xerj/issues/941)). A new roadmap
  section and [docs/ZERO_TOKEN_DIRECTION.md](./docs/ZERO_TOKEN_DIRECTION.md)
  lay out judged search, share links and a guest reading room, mail ingest,
  semantic detections, a real object-storage backend, a block index mode for
  logs, user-code ingest plugins and a corpus hub of signed packs — and say
  plainly what exists today: `S3Backend` was a local-directory simulation and
  `storage.backend = "s3"` refuses to start on purpose — the first half of that
  is no longer true in this same release, see the object-storage backend entry
  above; the refusal stands, because the index segment path is still local —
  `_watcher` stores
  watches and never evaluates them, alert rules have schemas and no
  evaluator, `xerj-logs` has no caller, and there is no wasmtime backend in
  the tree. The measuring behind it
  ([benchmarks/neural-path-triage/](./benchmarks/neural-path-triage/)) filed
  four defects with literal reproductions:
  [#937](https://github.com/xerj-org/xerj/issues/937) a declared analyzer
  stops applying at flush, [#938](https://github.com/xerj-org/xerj/issues/938)
  neural ingest keeps ~3.4 of 32 threads busy,
  [#939](https://github.com/xerj-org/xerj/issues/939) `semantic` over
  multi-passage documents is an exact scan that copies every `_source`
  (~410 ms p50 on 5,183 documents, forward pass ~14 ms), and
  [#940](https://github.com/xerj-org/xerj/issues/940) tied RRF scores change
  order across a restart. All four stay open; nothing is fixed by this entry.

### Fixed

- **A `hybrid` query returns the same page on every run and after a
  restart** ([#940](https://github.com/xerj-org/xerj/issues/940)).
  `fusion: "rrf"` and `fusion: "linear"` drained their accumulator from a
  hash map into a score-only sort, so documents with an identical fused
  score came back in hash-iteration order, which is seeded per process.
  Under RRF such ties are structural (one leg's rank *r* and the other
  leg's rank *r* score the same), and on BEIR SciFact 21 of 40 queries
  changed their top 10 after a restart on unchanged data. Fused results now
  use the order every score-ranked page uses since #270: score descending,
  then arrival (`_seq_no`), then `_id`; a NaN fused score sorts last
  instead of making the sort panic. Measured on the fixed build, lexical
  embedder: the 300 SciFact test queries (1,509 tied adjacent pairs in
  their top 100) returned identical result lists across a flush and two
  restarts. The hybrid recipe's worked example was also re-run and its
  numbers reconciled with the current engine.

## [1.0.0-rc.74] - 2026-09-08

### Added

- **`xerj init` wires XERJ into the project's coding agents in one command,
  and `xerj gain` reports counted usage**
  ([#917](https://github.com/xerj-org/xerj/pull/917)). `init` detects the
  agent surfaces present and writes the minimal config for each — the MCP
  server merged into `.mcp.json` (other servers kept, absolute binary path),
  a ten-line skill for Claude Code, a `.cursor/rules` file when `.cursor/`
  exists, and an `AGENTS.md` section — taking a `.bak` backup of any file it
  edits, previewable with `--dry-run`, idempotent on re-run, and refusing to
  touch an invalid `.mcp.json`. It deliberately does not rewrite prompts or
  intercept commands: it only makes the tool visible. `gain` reads the
  node's hash-chained audit log and prints what actually happened — searches
  served, hit rate, p50/p95 latency, busiest indices, a recent-search strip —
  with no estimated-savings arithmetic of any kind; it falls back to the
  native listener (ES port + 1) where `/_audit/_search` is served.

## [1.0.0-rc.73] - 2026-09-08

### Added

- **`xerj search` now runs a definition-first query, and `xerj def` is
  go-to-definition** ([#916](https://github.com/xerj-org/xerj/pull/916)).
  The search CLI's single `multi_match` found the correct cross-file source
  file at rank 1 only 38% of the time on an 80-task benchmark; a bool that
  adds a phrase clause on the AST `defs` field plus an exact-symbol `term`
  on `name` measures 94% on the same corpus, so that shape is now the
  default. Results are passage-first — the matching snippet and the symbol
  declaration, never whole file bodies (~9x fewer tokens across three code
  hits) — and a zero-hit identifier-shaped query nudges toward the new
  command. `xerj def "<symbol>"` answers the sharper question in one call:
  where is this defined and what is its signature, as `file:line [kind]` +
  declaration, with `--kind`/`--lang` filters; per-symbol documents
  ([#500](https://github.com/xerj-org/xerj/issues/500)) first, a `defs`
  phrase fallback for indexes that predate them. The MCP `xerj_search` tool
  accepts a plain string `query` and applies the same shape and passage
  projection (it previously rejected strings with an HTTP 400, forcing
  agents to hand-write query DSL); DSL objects pass through unchanged.

- **`autoindex`'s run summary reports throughput as `docs/sec`**
  ([#904](https://github.com/xerj-org/xerj/issues/904)). Split out of
  [#366](https://github.com/xerj-org/xerj/issues/366), which asked for this
  so an operator could see ingest throughput without timing the command by
  hand or inferring it from the elapsed line; [#897](https://github.com/xerj-org/xerj/issues/897)
  fixed that issue's neural-embedder batching defect but did not touch the
  summary output. The rate is `records_total / wall` (0 when `wall` is
  0.0, e.g. an all-resume no-op, rather than reporting `inf`), printed on
  the `done in …` line and added to the `--json` run document as
  `docs_per_sec`.

### Performance

- **Merge copies posting lists instead of re-analysing every document it
  merges** ([#876](https://github.com/xerj-org/xerj/issues/876)). A merged
  segment's postings ARE its inputs' postings with the document ids remapped —
  nothing about them depends on the source text — but the merge rebuilt each
  side-car by walking back to every surviving document's stored `_source`,
  re-extracting its field values and re-running the whole analyzer chain.
  Under size-tiered levelling that re-analyses every document once per level,
  which is why a post-index merge tail could rival the index itself. The merge
  now streams each input's term dictionary, replays the decoded posting lists
  with the doc ids remapped and tombstoned documents skipped, and carries the
  norms and field statistics across.

  Measured on a 100 MiB text corpus force-merged to one segment (2 000
  documents, 32-core box, median of four runs per arm): the merged segment's
  FTS side-car build drops from **3.15 s to 0.85 s (~3.7x)**, and peak RSS
  over the whole force-merge from ~2.8 GiB to ~2.4 GiB. End-to-end force-merge
  time moves less, ~41 s to ~32 s, and is noisy — because after this change the
  merge is dominated by the doc-values column build (7-12 s per batch against
  0.85 s for FTS), which this change does not touch.

  Everything a query can read out of a merged segment is identical to what
  the old path wrote — every posting list, `doc_freq`, norm byte, `_score`
  and `field_length` — and the `.fst` and `.post` side-cars are byte-for-byte
  identical. Two on-disk representations differ without any query being able
  to tell:

  - The `.norms` array may be SHORTER. Byte 0 spells both "field absent" and
    "field length <= 1", and the reader drops byte-0 entries, so a trailing
    run of such documents cannot be replayed and the dense array stops at the
    last non-zero document instead of at the last document carrying the
    field. A single-token `keyword` field is entirely byte 0, so its `.norms`
    shrinks to its header. Both files load to the same norms table.
  - `.meta`'s `total_term_frequency` for a docs-only (`keyword`) field: a
    document that repeats the same value merges with
    `total_term_frequency = doc_frequency`, because that format never stored
    a per-document frequency and its reader synthesises 1. Nothing outside
    `xerj-fts` reads `total_term_frequency`; `.meta` is otherwise identical,
    `total_field_length` included, so `avgdl` still counts the repeat.

  Two further edges exist only when a merge also DROPS documents: a dropped
  document whose value analysed to zero tokens leaves no trace to reclaim its
  `total_docs` seat, and a dropped docs-only document's length is recovered
  from the quantised norm byte, exact to length 7.

  This also changes one behaviour worth stating plainly: because postings are
  now preserved as written, a merge no longer re-analyses old segments under a
  changed mapping. Editing an analyzer and running `_forcemerge` used to
  rewrite the old documents' terms as a side effect; it no longer does — that
  needs a reindex. A change to a field's POSITION setting is still detected
  and still falls back to the rebuild; an analyzer-only change is not
  detectable and is not re-applied. This matches Lucene's semantics.

  A merge falls back to the old rebuild — same output, old cost — when the
  replay cannot be proved equivalent: an input whose side-cars will not open
  or enumerate, an input carrying documents but no FTS data, or a field whose
  stored position setting disagrees with what the merge would write.
  `Index::merge_reanalysed_document_count()` reports how many documents took
  that fallback, `XERJ_PROF=1` prints a per-batch `merge-batch` phase
  breakdown, and `XERJ_MERGE_FTS_REANALYZE=1` forces every merge back onto the
  old path.

- **`--embed-mode neural`: a long passage no longer pads a whole batch up to
  its own length** ([#366](https://github.com/xerj-org/xerj/issues/366)). The
  built-in Candle encoder tokenized every call with `PaddingStrategy::
  BatchLongest` and ran one rectangular forward pass over it, so a window that
  held one 512-character chunk beside sixty short lines cost `61 × long`
  instead of `long + 60 × short` — the model did the work, the attention
  tensors were allocated, and the padding contributed nothing. The encoder now
  sorts a call's passages by token length and cuts batches at 64 rows or a 4096
  `rows × padded_sequence_length` budget (`xerj-ai/src/microbatch.rs`, shared
  with the experimental ONNX backend), pads each batch to its own longest row,
  and restores input order. This affects the opt-in neural backend only; the
  default embedder is unchanged lexical feature-hashing.

  Measured on this repo's 32-core x86_64 box with MiniLM-L6 (median of three
  interleaved runs each, box shared with other builds — the spread is wide, so
  the medians are quoted, not the best runs):

  | shape | before | after |
  |---|---|---|
  | one 210 KB source file (504 chunks, one call) | 46.2–57.8 s, 8.97 GB peak RSS | 19.9 s, 0.42 GB peak RSS |
  | 45 files of `apache/lucene`, chunked as ingest chunks them (1214 passages) | 8.9 passages/s | 12.1 passages/s |
  | synthetic window of 4 long chunks + 60 short lines | 40.2 passages/s | 145.7 passages/s |
  | uniform short documents, window of 64 | 171.0 passages/s | 175.6 passages/s |
  | one passage per call | 68.9 passages/s | 72.8 passages/s |

  Padding waste (`rows × padded_sequence_length` ÷ real tokens) on the Lucene
  corpus fell from 1.50 to 1.13, and on the synthetic mixed window from 2.66 to
  1.00.

  The first row is the other half of #366 — "each of those two files held the
  run for 30+ seconds" on 200 KB C++ headers. `semantic_embedding_window_end`
  always admits a whole document even past the scheduling window, so every
  chunk of a large field reached the model as one forward pass whose attention
  tensors scaled with the document: 8.97 GB of resident memory for a single
  210 KB file, which is an OOM on a laptop rather than a slowdown. The row cap
  bounds it.

  **What this does not fix, stated plainly:** #366 reported ~15 documents/s on
  short uniform strings and read it as per-document inference. That reading
  does not hold — HTTP `_bulk` already batches (`bulk.rs` collects the whole
  request, `index.rs` windows 64 passages across documents), and the last two
  rows above show that shape is unchanged by this work. On uniform short
  documents ~15 docs/s is the cost of CPU transformer inference, and moving it
  needs quantization, a GPU provider or a smaller model. What was genuinely
  broken was mixed-length work, which is what pointing `autoindex` at a real
  folder produces. Binary-protocol `_bulk` embedded one document per inference
  call when this was written (`xerj-api/src/binary_protocol.rs`); that half is
  the entry below.

- **Binary-protocol `_bulk` shares one embedding pass across the batch instead
  of one inference call per document**
  ([#903](https://github.com/xerj-org/xerj/issues/903), split out of #366).
  `handle_bulk` looped `index_document` per document, and `index_document`
  embeds its own `semantic_text` fields — so a 64-document bulk cost 64
  inference calls of one passage each. It now collects the batch and hands it
  to `Index::index_documents_batched`, which routes it through the same
  `apply_semantic_embeddings_batch` entry the HTTP `_bulk` path uses: passages
  are windowed at `embedding.onnx_scheduling_window` (default 64) across
  documents, publication stays per document, and one document's failure still
  fails only that document. There is no second batching rule.

  Measured end to end over a real TCP connection through the handler with the
  harness added here (`cargo run --release -p xerj-api --features neural
  --example binary_bulk_throughput -- <documents> <per-bulk>`), on this repo's
  32-core x86_64 box with `--embed-mode neural` (built-in Candle backend,
  `sentence-transformers/all-MiniLM-L6-v2`). Corpus: 1 000 lines of
  `apache/lucene`'s `lucene/CHANGES.txt`, 40–400 bytes each so every document
  is exactly one passage. Medians of interleaved runs; the box was shared with
  other builds, so the spread is wide and the medians are quoted, not the best
  runs:

  | documents per bulk | before | after |
  |---|---|---|
  | 16 (n=3) | 60.7 docs/s | 82.6 docs/s |
  | 64 (n=5) | 64.0 docs/s | 102.7 docs/s |
  | 256 (n=3) | 84.3 docs/s | 124.9 docs/s |

  **Bounds, stated plainly.** The win is a short-document win. A document long
  enough to fill a scheduling window on its own already got its own inference
  call: `semantic_embedding_window_end` admits a whole document even past the
  window (`index.rs`), so before and after are the same call for it. On the
  default lexical feature-hashing embedder there is no inference to batch and
  no change was measured — 872 → 865 docs/s (medians of n=7 interleaved,
  ranges fully overlapping). And the module this fixes is a library entry
  point: `serve_binary_protocol` is not bound by any listener in `xerj-server`
  (`xerj-engine/src/index_guard.rs` says so, and nothing in the tree calls
  it), so no shipped-server ingest path changes speed with this release.

### Fixed

- **`knn` beside a `query` is answered from the inverted index again — and its
  page no longer drops the documents matched by both halves**
  ([#892](https://github.com/xerj-org/xerj/issues/892)). This closes the
  regression rc.72 shipped knowingly: the
  [#825](https://github.com/xerj-org/xerj/issues/825) fix pre-executes the
  vector leg and pins its top-`k` into the query tree as `constant_score{ids}`
  clauses, `_id` is not in the FTS term dictionary, so the projection declined
  the whole tree and every segment fell to a stored-document scan.

  That route was not only slow. The scan admits documents in stored-layout
  order and stops materialising once it reaches the page cap — no score
  comparison — so on any index with more lexical matches than that cap, the
  documents reached by BOTH halves, the ones ES scores `query_score +
  knn_score` and ranks first, never entered the page. `hits.total` was right;
  the page was not. Live-verified on 100 000 documents: for a `match` + `knn`
  k=10 request whose vector top-10 held exactly two lexical matches, rc.72's
  top 20 was twenty lexical-only documents tied at 1.693 and neither
  both-halves document appeared at all. They now rank 1 and 2, at 3.153 and
  3.144.

  The projection now lifts the pinned disjunct to an FTS leaf carrying
  `(doc_id, score)` pairs, resolved per segment through the same `_id` →
  stored-position index that `ids` queries and `_mget` already use. The
  lexical half keeps the inverted index and exact BM25, the vector half keeps
  its own scores, and the `bool` sums them exactly as before.

  Measured on that corpus (`text` + 8-dim `dense_vector`, ~10 % lexical
  selectivity, bulk-loaded once and never updated — which is the only state in
  which this route engages, see the scope note below — closed-loop, fresh query
  vector per request so the query cache cannot answer twice, medians of 2
  rounds × 7 requests, on a shared build machine — the two control rows bound
  the noise):

  | request | rc.72 | with this fix |
  |---|---|---|
  | `query` alone (control) | 0.45 ms | 0.52 ms |
  | `knn` alone, k=10 (control) | 1.99 ms | 2.12 ms |
  | `query` + `knn` k=10 | 265.9 ms | **6.4 ms** |
  | `query` + `knn` k=100 | 326.9 ms | **8.4 ms** |
  | `query` + `knn` k=1000 | 505.0 ms | **27.5 ms** |
  | `query` + `knn` + `aggs` k=10 | 784.1 ms | **534.4 ms** |

  `hits.total`, the aggregation buckets and the field-sorted page came back
  identical across the two arms — one request shape per column, compared as
  JSON — and so did `query`-alone and `knn`-alone. The agg row keeps most of
  its cost because a `terms` agg over this shape still materialises the whole
  corpus — a different path, untouched here.

  Two things to know:

  - **`_score` values change for this shape.** The lexical half is now scored
    by exact BM25 — the same number the identical query returns *without* a
    `knn` beside it — instead of the stored scan's heuristic. Ordering follows
    the #825 contract as before; absolute values move, so a `min_score` tuned
    against rc.72's hybrid needs revisiting.
  - **The indexed route applies only to an index that has never taken an
    overwrite or a delete** — that is the scope of every number above, and it
    is a one-way door, not a transient window. The gate is the one the `ids`
    pre-filter already applies: the per-segment `_id` index keeps one position
    per id, so a segment physically holding a document twice (an overwrite
    flushed alongside its predecessor) could hand back the superseded copy. It
    reads `VersionMap::ghost_events`, which is **monotonic by design** — it
    counts overwrite and delete events for the life of the open index and is
    never decremented, not even by the merge that purges the superseded copies.
    One `PUT` over an existing `_id`, or one `DELETE`, therefore turns this
    route off for that index until it is re-opened, and it stays off across
    that re-open while superseded copies remain on disk. An index that takes
    updates or deletes keeps rc.72's stored scan for `knn` beside `query` —
    still correct, and no slower than it was, but with none of the speedups
    above. Append-only indexes — bulk load, reindex-and-swap, log and event
    corpora — get them. Widening the route to ghost-bearing indexes is
    separate work: it has to prove the #825 union/sum contract with tombstones
    present, and is not attempted here.

- **A `date_nanos` field no longer guesses its epoch scale value by value, and
  an index written before that flag existed recovers its scale instead of
  guessing** ([#889](https://github.com/xerj-org/xerj/issues/889), follow-up to
  [#790](https://github.com/xerj-org/xerj/issues/790)). #790 made a date
  field's epoch scale a property of the MAPPING and pinned a declared `date` to
  milliseconds, but deliberately left `date_nanos` on the old per-VALUE guess
  (">= 4 fractional-second digits means nanoseconds"). The defect #790
  describes therefore survived inside `date_nanos`: a column holding both
  `2020-06-06T06:06:06.123456Z` and `2025-01-01T00:00:00Z` gave the first a
  nanosecond key (~1.59e18) and the second a millisecond one (~1.74e12), six
  orders of magnitude apart, so 2025 sorted before 2020 and a range bound could
  not reach both.

  Pinning `date_nanos` to nanoseconds is not safe on its own — it moves a
  `date_nanos` field six orders of magnitude away from a `date` field in a
  cross-index sort — so the prerequisite landed with it:

  - **ES `numeric_type` on a field sort** (`numeric_type: date` /
    `date_nanos`). It forces the epoch scale every participating index produces
    sort values on, which is the only way a sort over a field mapped `date`
    here and `date_nanos` there compares chronologically. The `search_after`
    cursor is normalised on the same forced scale, so paging works in that one
    space. `long`/`double` are parsed and ignored (xerj already compares
    numeric sort keys as `f64`), and unlike ES, xerj does not reject
    `numeric_type` on a non-numeric field. Without `numeric_type` the raw longs
    are compared, exactly as ES does and as
    `tests/es-compat-yaml/yaml/search/240_date_nanos.yml` asserts — that
    default is unchanged and is pinned by a test.
  - **`date_nanos` is now uniformly nanosecond-scaled**, including day-first
    slash dates (`dd/MM/yyyy HH:mm:ss.SSS`), which previously had no nanosecond
    branch at all and would have re-introduced the mixed-scale split by a
    different route.
  - **A NUMERIC date bound is epoch-MILLISECONDS and is converted at the
    bound, not at the value.** That is ES's own rule — a `date_nanos` field's
    default format is `strict_date_optional_time_nanos||epoch_millis`, so a
    bare number is parsed as milliseconds and only then moved to the field's
    resolution. Moving a `date_nanos` column to nanosecond keys therefore had
    to move every numeric bound with it: the segment sort-shadow's bounds
    (`shadow_range_bounds`, which serves both the range doc-values pre-filter
    and the `_count` shortcut) took a numeric bound verbatim, six orders of
    magnitude below the keys it bisects. Two shapes reach it — the #788
    `term`-on-a-date rewrite, which lowers a `term` to the degenerate
    `range [epoch_ms, epoch_ms]`, and a user-written numeric bound — so on a
    flushed `date_nanos` index a `term` and a numeric range both matched
    nothing while the same query answered correctly from the memtable. Bound
    and key are now on one scale in both directions, and the flush-invariance
    of both shapes is pinned by a test that asserts pre- AND post-flush.
  - **One format ladder for both scales.** `date_string_to_epoch` had a
    separate nanosecond branch that stopped after RFC3339 and
    `%Y-%m-%dT%H:%M:%S%.f`; the `%Y-%m-%d` date-only fallback existed only in
    the millisecond branch. A DATE-ONLY value ("2021-01-01") in a `date_nanos`
    field therefore normalised to nothing at all and was emitted as a raw
    JSON STRING among i64 epoch keys, where the sort comparator's string arm
    puts `"` below every digit and the document sorted FIRST ascending
    whatever its instant — and the segment's sort/range shadow refused to
    build on that term. Because `numeric_type` forces the nanosecond scale on
    every participating index, an ordinary `date` field holding a date-only
    value hit the same hole under a cross-index sort. The ladder is now walked
    once and the accepted instant rendered on the requested scale at the end,
    so a format either parses on both scales or on neither. The `search_after`
    cursor normaliser, which carried a hand-copied second copy of the same
    ladder whose fallbacks returned milliseconds even on a nanosecond field,
    now calls the one function.
  - **Legacy indices are back-filled at open.** `date_precision` is recorded
    when a field is mapped, so an index created by an older build comes back
    with no precision at all and every date field falls back to the per-value
    guess. `schema.json` cannot tell `date` from `date_nanos` — but the raw ES
    mapping blob beside it (`es_mapping.json`) still carries the original type
    string, and `Index::open` now recovers the flag from there. Both directions
    are covered: a legacy `date_nanos` comes back nanosecond-scaled (a blanket
    default would have silently downgraded it to millisecond resolution) and a
    legacy `date` comes back millisecond-scaled, i.e. it inherits the #790 fix
    without a reindex. Nothing on disk is rewritten and no data is reindexed:
    the epoch keys were always derived at read time from the stored value, so
    correcting the scale is enough.
  - **`GET _mapping` reports a `date_nanos` field as `date_nanos`** where the
    mapping is derived from the schema (it already round-tripped correctly when
    the raw blob was present). The GET → PUT round trip every reindex helper
    performs no longer silently downgrades the field.

  **Not fixed, stated plainly:**

  - An index whose `schema.json` predates the flag AND which has no
    `es_mapping.json` (an index that never went through the ES-compat mapping
    path) still has no record of `date` vs `date_nanos`, so its date fields
    keep the legacy per-value guess. Inventing a default there is the silent
    downgrade this change exists to avoid.
  - The back-fill reads `{"properties": …}` and `{"mappings": {"properties":
    …}}`, which is what the mapping API persists. An ES-6-style type wrapper
    (`{"_doc": {"properties": …}}`) is skipped, and that field then keeps the
    legacy per-value guess — the same safe fallback, not a new failure mode.
  - Only STRING-shaped date values move onto the field's scale. A numeric
    source value (`{"ts": 1591423566123}`) in a `date_nanos` field stays a raw
    millisecond number in its doc-values column; ES would convert it. A
    segment holding both shapes ships no column at all and falls to the
    correct full scan, but two segments that each hold one shape still mix
    scales in a sort.
  - `numeric_type` values other than `date`/`date_nanos`/`long`/`double` are
    lower-cased and then ignored where ES answers 400, and `numeric_type` on a
    non-numeric field is accepted where ES rejects it.
  - Sort-value FORMATTING still infers milliseconds vs nanoseconds from the
    magnitude of the epoch number rather than from the resolved scale
    (`format_sort_value`, threshold `2 * 10^15`). Unchanged by this work: it
    reads correctly for every nanosecond instant from 1970-01-24 on
    (2e15 ns = 2e6 s = 23.1 days) and every millisecond instant below 2e15 ms
    (~63,000 years past the epoch).

- **`autoindex`: a run no longer deletes a sibling corpus's duplicate-alias
  catalog documents** ([#905](https://github.com/xerj-org/xerj/issues/905)).
  Data loss, and the second of the two holes the #890 review found.
  `autoindex-catalog` is one index shared by every corpus on the node, but the
  duplicate-alias sweep issued at the end of a run was
  `{"bool": {"filter": [{"term": {"status": "duplicate"}}, {"terms": {"path":
  [...]}}]}}` with **no corpus scope at all** — and `path` is a logical path
  inside a corpus, not a node-unique key. Two checkouts on one node that each
  carry `LICENSE` and `vendor/LICENSE` is the ordinary shape of that, and a
  delete is not recoverable. Unlike #890 this needed no legacy mapping: it was
  unscoped on a current `keyword` catalog too.

  The query still selects the candidates — it is what keeps the candidate set
  small — but the scope is now applied per hit and the deletes go by `_id`, the
  pattern #890's `delete_catalog_docs_scoped` introduced (both sweeps share one
  page walk now, so there is one copy of its paging rules rather than two). A
  simple scope filter would have traded one defect for another, which is why
  this was split out of #896 rather than folded into it: the sweep exists
  *because* alias ids changed as identity evolved, and its oldest targets
  (v1.0.0-rc.10..rc.56, before #416 put the corpus prefix into the `_id`) carry
  no corpus field and no prefix in their id, so a server-side
  `corpus_scope`/`prefix` term would simply stop matching them. Attribution is
  therefore layered: `corpus_scope` (#755), else `prefix` (#737) read from raw
  `_source`, else the prefix in the `_id` (#416). Below that the document names
  no corpus at all, and rather than guess in either direction the sweep deletes
  such a hit only when its `_id` is one this corpus itself would have written
  under the pre-#416 scheme — the reconstruct-the-id move #739 already makes —
  and otherwise leaves it in place and says so, by path and id, in a new
  non-fatal note. The sweep stays non-fatal (#345) throughout.

  Scope, stated rather than implied: this is the legacy (graph) run path only.
  A `--no-graph` run returns from the generated path long before this sweep and
  retires stale catalog documents by their prefix-scoped `_id`
  (`generation_catalog::managed_non_run_ids`), which was never unscoped.

- **`--embed-mode neural`: re-indexing an unchanged corpus no longer fails,
  and the identity can now tell one model from another**
  ([#487](https://github.com/xerj-org/xerj/issues/487),
  [#367](https://github.com/xerj-org/xerj/issues/367)). The neural arm of the
  embedding execution identity hashed the configured model *name*
  (`neural-unpinned.v1;configured-model=…`) and reported `resumable: false`
  unconditionally. Two consequences, both reproduced on the released rc.71
  binary before the change:

  - every neural node on earth emitted the same `identity_sha256`
    (`6e51e5ce3e46…`), so the check that exists to catch a swapped model could
    not see one; and
  - the five `resumable` gates in `xerj-autoindex` all fired. `xerj autoindex`
    against a neural node was **one-shot**: the graph path completed
    generation 1 (`exit=0`) and then refused the *identical* command over an
    *unchanged* corpus (`exit=1`, "server embedding backend cannot safely
    resume semantic indexing"), and the `--no-graph` path refused even the
    first run at the generation cutover. That is the whole reference-coding
    loop — index a peer repo, pick up its commits later — so it mattered more
    than a bookkeeping error looks.

  The identity is now the streamed sha256 of `model.safetensors`,
  `tokenizer.json` and `config.json` plus the model's own `hidden_size`,
  pooling and truncation limit, resolved by exactly the path
  `NeuralEmbedder::load` takes (`xerj-ai/src/neural.rs`) — the same thing the
  ONNX arm already did. `resumable` is then honestly `true`, `dimensions` is
  reported instead of omitted, and a genuinely swapped model is refused where
  before nothing was. Assets that cannot be resolved still fail closed, but the
  reason names the file rather than blaming a configuration change that never
  happened.

  Measured on this box, 20-file corpus, `--embed-mode neural` (MiniLM from the
  local HuggingFace cache):

  | run | rc.71 | this change |
  |---|---|---|
  | graph path, run 1 | `exit=0` | `exit=0` |
  | graph path, run 2 — identical command, unchanged corpus | `exit=1` | `exit=0` |
  | graph path, run 3 — one file edited | not reachable | `exit=0` |
  | `--no-graph`, run 1 | `exit=1` | `exit=0` |
  | `--no-graph`, run 2 — unchanged corpus | `exit=1` | `exit=0` |

  The pre-fix identity was already *deterministic* — the same digest on every
  call and every host — so this was never a hashing-stability bug; the flag was
  simply hardcoded. Batch shape (`MAX_BATCH_ROWS`, `PADDED_TOKEN_BUDGET`),
  thread count, device and the resolved file paths are deliberately **not** in
  the digest: they change how rows are grouped, not what any row computes, so
  folding them in would have reintroduced the false "identity changed" one
  layer down.

  On-disk migration: `embedding_execution.json` moves to version 3, so the
  first reopen of a populated neural index after upgrading is grandfathered and
  re-baselined instead of refused — the same handling #533 gave the proxy
  endpoint change. `proxy` is unchanged and still reports `resumable: false`:
  a remote provider attests nothing about its model's bytes, and probing it
  with fixed inputs would hash floats that no provider guarantees to be
  reproducible — which is the bug in this issue, one layer down.

- **A frontmatter block's body is judged as YAML by whether it parses, not only
  by how its lines look**
  ([#587](https://github.com/xerj-org/xerj/issues/587)). The `---` lookahead
  added for [#551](https://github.com/xerj-org/xerj/issues/551) decided "is the
  text after the closing `---` YAML?" from line shape alone — a body was YAML
  if most of its lines read as `key:` or `- item`. Two shapes fell through it.
  A genuine multi-document YAML stream whose later document is dominated by
  block scalars, comments and indented continuation lines has almost no
  `key:`-shaped lines, so the whole stream was re-classified as a text family
  and lost its per-document key structure; and the lookahead required a body of
  at least two non-blank lines, so a document whose body was a single line
  still went to the YAML extractor, which keeps the frontmatter mapping and
  drops that line. `classify_text` now runs the body through
  `classify_structured` first (JSON is valid YAML, so this ordering matters),
  then asks whether the body actually parses as a YAML stream of mappings or
  sequences — markdown-with-frontmatter does not parse as one, a real stream
  does — and only falls to the prose families when it does not
  (`xerj-autoindex/src/sniff.rs`).

  Measured over the 36,936 files in this box's reference corpora and local
  checkout whose first non-blank line is `---` (279 distinct by content
  changed): 69 files moved into `Yaml` — every one of them a `.yml`/`.yaml`
  file, including Elasticsearch's `yamlRestTest` specs and Cilium manifests
  that had been demoted to line-records — and 205 moved out of `Yaml` into
  prose plus 5 into `Json`, every one of them a `.md`, `.mdx` or `.snap` file
  whose one-line body the YAML extractor was discarding. No markdown file moved
  into `Yaml`. (Extensions are never consulted by the sniffer, so they are an
  independent check on the direction of each change.)

- **A legitimate authenticated request whose body arrived in small HTTP/2 DATA
  frames could be cut off with `GOAWAY ENHANCE_YOUR_CALM`**
  ([#485](https://github.com/xerj-org/xerj/issues/485)). The rc.18 `h2` bump for
  RUSTSEC-2026-0258 — described at length in that release's notes below — added
  a per-connection budget for small DATA frames: a frame under 256 bytes
  consumes `256 - payload_len`, a frame of 256 or more replenishes, and the
  refund for a small frame lands only when the application *reads* it. In `h2`
  0.4.16 that budget was a fixed 25,600 bytes however large a window the server
  had advertised, while hyper advertises a 1 MiB HTTP/2 connection window — so a
  well-behaved client could have 40× the budget in flight before the handler was
  ever scheduled. Any client that chunks small (a streaming JSON encoder, a
  per-document flush, a gRPC stream of small messages) lost its whole
  *connection* on a request that would otherwise have returned 200.

  `h2` is now floored at 0.4.19 in `[workspace.dependencies]`
  (`engine/Cargo.toml`), where the budget is half the configured connection
  window with 25,600 bytes as the minimum — 512 KiB against hyper's 1 MiB
  window — and the empty-DATA-frame flood the advisory was actually filed about
  is capped by its own counter (100 frames per connection) rather than out of
  the byte budget. The mitigation is not disabled or weakened here, and no
  XERJ-side window or budget knob was changed; the floor sits in the workspace
  manifest so `cargo update` cannot resolve back to a fixed-budget release.

  Measured on this box, same harness on both sides (`axum::serve`, hyper 1.9.0,
  a handler that reads the whole body, one DATA frame per chunk queued
  back-to-back with no pause), bisecting for the largest body that completes.
  The boundary is not a constant — it is a race between arriving charges and
  the refunds a reading handler returns — so these are the min–max of three
  bisections each, not thresholds to design against:

  | DATA frame size | before (h2 0.4.16) | after (h2 0.4.19) |
  |---|---|---|
  | 64 B | 9.3–16.5 KB (145–258 frames) | 262 KB–6.4 MB (4,095–99,793 frames) |
  | 100 B | 18.9–27.2 KB (189–272 frames) | 410 KB–4.4 MB (4,095–43,561 frames) |
  | 128 B | 29.3–36.6 KB (229–286 frames) | 2.1–4.7 MB (16,127–36,628 frames) |
  | 200 B | 141–419 KB (703–2,094 frames) | no cut-off up to 52 MB |
  | 256 B | no cut-off up to 8 MB | no cut-off (never charges) |

  Where the safe line now sits, exactly: with a budget of half the window, a
  frame of **171 bytes or more** cannot exhaust it even if the application never
  reads a byte — at most `window / size` frames can be in flight, each charging
  `256 - size`, and `(256 - size) ≤ size / 2` from 171 up. That is why the
  200 B row has no ceiling at all. Below 171 bytes there is still a ceiling and
  it still depends on how fast the handler drains; it is now one to two orders
  of magnitude further out. A frame of 256 bytes or more never charges, before
  or after. The rc.18 note advising clients to chunk at 256 bytes or above is
  no longer needed for ordinary bodies.

  Deliberately abusive traffic is still refused, and that is asserted rather
  than assumed: a flood of empty DATA frames still ends in `GOAWAY
  ENHANCE_YOUR_CALM` (upstream's 100-frame cap, unchanged), and a flood of
  1-byte frames is still cut off — on this harness the client saw the
  connection go at roughly frame 4,600 instead of roughly frame 100.

  What was measured where. The table above comes from a standalone harness
  pinned to this workspace's own axum/hyper/h2 versions, not from the `xerj`
  binary. The binary itself is covered by
  `engine/crates/xerj-server/tests/http2_small_data_frames.rs`, which spawns the
  real server with authentication on and drives a 40 KB authenticated `_bulk`
  at 100 bytes per DATA frame (401 frames) through the ES-compat listener over
  prior-knowledge h2c, asserting a 200 with every document indexed. Against the
  unfixed dependency that same test is answered
  `GOAWAY ENHANCE_YOUR_CALM (too_many_data_frames)` mid-body, which is how it
  was checked. A second test holds the empty-frame guard in place. The TLS
  listener and the gRPC listener sit on the same `h2` and take the same fix,
  but neither is exercised by these tests.

- **`xerj autoindex --no-graph` no longer aborts with `Access denied
  (os error 5)` on Windows**
  ([#482](https://github.com/xerj-org/xerj/issues/482)). The durable-snapshot
  path fsynced the directories it had just published with
  `File::open(dir)?.sync_all()?`. That is a Unix-only idiom: on Windows
  `File::open` on a *directory* returns `ERROR_ACCESS_DENIED` (os error 5) on
  every call, because `std` cannot pass `FILE_FLAG_BACKUP_SEMANTICS`. So the
  first such call — sealing the source snapshot, immediately after the journal
  was written and before a single document was indexed — killed the run with a
  context-free, locale-dependent io error (`Acceso denegado. (os error 5)` in
  the report). A second, unconditional open of the same kind in snapshot GC
  then failed *every later run* over that state directory, whether or not
  `--no-graph` was passed again. `xerj-autoindex` now routes directory
  durability through `xerj_common::fsio::fsync_dir`, which has carried the
  documented `#[cfg(windows)]` shim since the same mistake stopped the server
  creating any index at boot; the surrounding filesystem calls also carry
  context now, so a future failure names the operation instead of only its
  errno. This costs Windows no durability it had: the code it replaces flushed
  nothing there — it returned `Err` and ended the run. Windows keeps the
  engine-wide posture that directory-namespace changes carry no durability
  claim; file contents inside a snapshot are still fsynced. The
  windows-latest `autoindex-fd-smoke` CI job now runs the reported
  `--no-graph` flow twice over one state directory, which is what would have
  caught this: the job existed, but only ever ran the default graph path,
  which never touches `sync-snapshots/`.

- **A code symbol's retrievable unit is the whole declaration, not the single
  physical line the name sits on**
  ([#500](https://github.com/xerj-org/xerj/issues/500)). Promoting each
  declaration to its own document
  ([#579](https://github.com/xerj-org/xerj/pull/579)) closed the
  constant-and-field half of that issue. The stored `code` was still literally
  `lines[name_row]`, so anything that did not fit on that one line came back
  wrong: a wrapped signature returned `public void configure(`; a declaration
  whose modifiers sit above the name (`static unsigned long\nhash_bytes(…)`,
  `#[inline]\npub fn scan(…)`) returned the wrong line entirely; and a
  declaration sharing its line with the body (`void m(){ int local = 1; }`)
  returned implementation nobody asked for. `code` is now sliced from the
  declaration's first token — leading attributes included — to where its body
  begins, so the unit is a complete signature rather than a fragment of one.

  Measured with the `decl_bench` harness added here (`XERJ_DECL_BENCH=<repo>
  cargo test --release -p xerj-autoindex --lib decl_bench -- --ignored
  --nocapture`), which computes the old unit and the new one side by side in one
  pass over the same parse and the same symbol set:

  | corpus | slice ending mid-signature, was → now | median bytes, was → now | the symbol's whole `line..end_line` span |
  |---|---|---|---|
  | Lucene — 5 627 Java files, 79 102 declarations | 4 137 (5.2 %) → 18 (0.02 %) | 49 → 52 (mean 50.5 → 60.7) | median 229, mean 1 046 |
  | valkey + memcached — 981 C files, 28 347 | 1 444 (5.1 %) → 54 (0.2 %) | 48 → 49 (mean 51.6 → 58.8) | median 123, mean 454 |
  | tantivy — 434 Rust files, 8 970 | 851 (9.5 %) → 27 (0.3 %) | 41 → 50 (mean 44.2 → 60.9) | median 218, mean 721 |
  | cilium + dpdk + vpp-agent + xdp-tools — 26 357 Go/C files, 914 561 | 87 267 (9.5 %) → 1 678 (0.2 %) | 49 → 53 (mean 54.6 → 67.4) | median 102, mean 387 |

  So the unit costs 7–17 bytes more on average and is a complete declaration
  instead of a fragment — still 1.9–4.4× smaller than that symbol's own span at
  the median and 5.7–17× smaller at the mean. 39.6 % of Lucene declarations wrap
  past one line (the single-line slice could only ever return a fragment of
  those) and 51.5 % have a name line that does not stop where the declaration
  does.

  Extraction is not free: medians of five interleaved runs over the same
  6 106-file Lucene tree, one box, `extract_bench` — 1 600 files/s before this
  change, 1 455 after. About 9 %, for an ancestor climb bounded at 12 levels and
  a pruned body search bounded at 4 096 nodes.

  Two honest limits. The cap is unchanged at 400 characters and a declaration
  that overruns it still falls back to the start line, so the WORST case per
  symbol is what it always was — but an individual document can grow, since a
  45-character name line may now store a 400-character signature. And "stops
  where the body begins" needs the grammar to say where that is: for a Haskell
  equation nothing marks it, so the slice is the whole equation. Corpus-wide
  that is 1.2–5.9 % of symbols whose slice covers their entire span, which
  `decl_bench` now reports as its own column rather than leaving to assertion.

  The file document's `symbols` sidecar gains `end_line`, so a caller that
  wants the implementation can address exactly `line`..`end_line` instead of
  guessing "up to where the next symbol starts" — the guess that returns a
  whole class for a class and the rest of the file for the last symbol.

  Upgrading does not rewrite what is already indexed: `xerj sync` reconciles by
  content digest, so an unchanged file produces no operation and its symbol
  documents keep the old single-line `code` and carry no `end_line`. An index
  upgraded to this release is a mixture until the files change or it is rebuilt
  from scratch.

- **The exclusion sweep can no longer delete a sibling corpus's catalog
  documents on a catalog an older build left `text`-mapped**
  ([#890](https://github.com/xerj-org/xerj/issues/890)). `autoindex-catalog` is
  shared by every corpus on the node, so when a widened ignore rule excludes a
  file, the sweep that removes its already-published documents is scoped to the
  corpus's `prefix`. On any catalog a v1.0.0-rc.15..rc.67 build wrote to before
  the mapping declared that field — which includes catalogs first created by an
  earlier release — it is dynamically inferred **`text`**, and a `term` query
  against an analyzed field matches the field's *tokens*: `prefix: "ax"` also
  reached documents whose prefix is `ax-2` (tokens `[ax, 2]`). Conjoined with
  the `path`/`file_key` a
  byte-identical file (a LICENSE, a lockfile) shares between corpora, the sweep
  deleted the **sibling corpus's live catalog documents** — the cross-corpus
  over-delete the scope was added in
  [#737](https://github.com/xerj-org/xerj/issues/737) to prevent, and a delete
  is not recoverable. Present since v1.0.0-rc.68.

  Both scoped catalog deletes now go through an exact scoped delete: the same
  query still selects candidates server-side, but every hit's raw scope values
  are re-checked against `_source` before it is deleted by `_id`. That is
  independent of the catalog's mapping, so it holds on a keyword catalog and a
  legacy text one alike, and it covers the `path`/`file_key` conjunct as well as
  the corpus scope. Coverage of the excluding corpus's own documents is
  unchanged, including alias documents the frozen plan does not name, which only
  this pass can reach. The candidate walk is bounded at ten pages of 1,000, and
  it ends on an exact `hits.total` as well as on a short page, so the boundary
  case of exactly 10,000 candidates completes rather than refusing. Past that
  bound it fails *closed* — it refuses with a `_reindex` remedy rather than
  reporting a removal it only partly made.

  Not fixed here: on a legacy catalog an analyzed field can also match *fewer*
  documents than it should, so a scoped sweep can still miss a document an older
  build wrote. That is the under-match
  [#755](https://github.com/xerj-org/xerj/issues/755) answers with the keyword
  `corpus_scope` field, and the run already warns with the `_reindex` that
  retires the legacy mapping.

- **CI's default-parallelism test step no longer races itself for a port**
  ([#751](https://github.com/xerj-org/xerj/issues/751), the second half). With
  the deadlock above fixed, the same step then failed on a real port race that
  the hang had been masking: `xerj-server`'s cleartext-bind tests allocated a
  port by binding `127.0.0.1:0`, reading the number, releasing it, and handing
  it to a child process that binds it later. The kernel is free to hand that
  same ephemeral port to the next `bind(":0")`, and several tests in that file
  keep a real server alive for seconds, so at default parallelism they
  overlapped — `gRPC: bind [::1]:46843 ... Address already in use`. Ports now
  come from a band below every platform's ephemeral range, strided per process
  and probe-bound on both `127.0.0.1` and `::1` (a port free on one family can
  be taken on the other, which is exactly how it failed). Test-only change.

- **A search that aggregates over a cold segment could deadlock forever**
  ([#751](https://github.com/xerj-org/xerj/issues/751)). On a multi-thread
  runtime `Index::search` runs its whole body inside
  `block_in_place(|| Handle::current().block_on(..))`: that hands the worker's
  core to a tokio *blocking-pool* thread — which then runs the scheduler loop
  for the life of the runtime — and parks the calling thread, itself a
  blocking-pool thread, for the entire search. An aggregation with no columnar
  fast path assembles the full corpus from inside that park, and the cold
  segment hydration it needs (`stored_values_for_async`) queued its decode with
  `spawn_blocking` — onto the very pool the waiter was holding a thread of.
  tokio grows that pool only when it observes zero idle threads at push time,
  so the core hand-off and the decode submission could both see the same idle
  thread, both merely notify, and the one thread that woke took the core and
  never returned. The decode was then queued behind threads that were all
  permanently running worker cores: the search waited for a task that could
  never be scheduled. The decode now runs on the engine's own rayon maintenance
  pool, whose threads are never consumed by tokio, so the wait cannot be
  circular. This is what made CI's default-parallelism workspace-test step hang
  on hosted runners and leave `main` with no verdict for hours. Measured on a
  4-cpu affinity mask with the reported test binary: 12 hangs in 60 runs before,
  0 in 200 after (and 0 in 100 on a 2-cpu mask). One consequence worth knowing:
  a cold hydration a search is waiting on now runs at the maintenance pool's
  `nice(10)` instead of a `nice(0)` blocking thread, which is only observable
  when every core is already saturated.

- **CI: every workflow job is bounded, so one hang can no longer cost `main` an
  afternoon of CI** ([#770](https://github.com/xerj-org/xerj/issues/770)). 22 of
  this repo's 23 GitHub Actions jobs declared no `timeout-minutes` and so ran
  against GitHub's 360-minute default. Three workflows serialise on a
  concurrency group that never auto-cancels (`ci-CI-refs/heads/main`,
  `pages-deploy`, `release-metrics`), so a single hung job holds that slot for
  six hours while every later run queues behind it. That is measured, not
  hypothetical. On 2026-08-25, before #767 capped `build-test`, the #751 hang
  ran that job into the 360-minute default and was killed by it (run
  32796557309, 01:21:28 → 07:21:44 — every other job in the same run finished
  within 18 minutes). A second hang the same morning held the slot from 11:28
  to 17:03, and the seven main pushes that landed behind it were all discarded
  with **zero jobs run**; a normal ~37-minute run would have let at least four
  of them through, since the gaps between them were 48, 62, 38 and 150 minutes.
  Every job now carries a cap sized in tiers from the measured duration of every
  job execution across the last ~245 CI runs (3 873 executions) and 27 release
  runs: 15 min for jobs that install no toolchain (measured max 2.3 min), 30 min
  for jobs that install a toolchain but compile no workspace member (max
  3.8 min), 60 min for jobs that compile workspace crates (max 48.3 min,
  `Build + Test`), and 75 min for the two jobs with the heaviest tails —
  `autoindex-fd-smoke`, whose `windows-latest` leg runs 18.8 min at the median
  but 49.6 at the maximum, and `release.yml`'s cross-compile matrix (max
  38.3 min, `aarch64-pc-windows-msvc`). Worst-case slot starvation drops from
  360 minutes to 75 (CI on `main`), 20 (`pages-deploy`) and 75 (a release tag).
  A new `.github/scripts/workflow-timeout-guard.py` keeps it true for jobs added
  later. This bounds the blast radius of the next hang; it is not itself a fix
  for any hang, and #899 remains what fixed #751's.

## [1.0.0-rc.72] - 2026-08-31

The idle-cost release. A 2026-08-29 measurement session found XERJ was not
quiet at rest: on a node holding 464 small indices — the shape `xerj autoindex`
produces from a reference corpus — the process burned 15–27% of a core and 115
wakeups per second with zero requests, and a 154 MB corpus spent 40+ minutes at
~110% CPU merging after ingest "finished". Customers reported it as fans
spinning on an idle laptop. Three of the four mechanisms are fixed here; the
fourth ([#876](https://github.com/xerj-org/xerj/issues/876)) has a published
design and is not yet implemented. The budget every fix is held to, and the
remaining gap, are tracked in the open meta-issue
[#874](https://github.com/xerj-org/xerj/issues/874).

Alongside it, six Elasticsearch-compatibility and autoindex correctness fixes,
each shipped with a test proven to fail on the unfixed code.

### Performance

- **An idle index no longer costs a timer, a thread or a wakeup**
  ([#871](https://github.com/xerj-org/xerj/issues/871)). Every index ran its own
  `tokio` task that woke every 5 seconds forever and evaluated merge policy
  whether or not anything had been written — 464 indices meant 93 wakeups per
  second and two runtime workers each burning ~10% of a core, on a node serving
  nothing. Merge scheduling is now event-driven, the same shape the WAL fsync
  scheduler adopted in rc.16 for the identical defect
  ([#334](https://github.com/xerj-org/xerj/issues/334)): a segments-changed hook
  fires on every snapshot swap that alters the segment set (flush publication,
  merge application, tombstone-only segment, orphan recovery) and a debounced
  request arms at most one check. `XERJ_MERGE_INTERVAL_SECS` keeps its meaning
  as the debounce delay. Measured at the unit level with an evaluation counter:
  768 evaluations across a simulated idle hour become 48.

- **The resource sampler stopped walking every index × every shard lock**
  ([#872](https://github.com/xerj-org/xerj/issues/872)). The memory breaker's
  sampler summed memtable bytes every 100 ms by taking a read lock on all 16
  shards of every index — 74,240 lock acquisitions per second at 464 indices, in
  one thread, to compute a number that is almost always zero. The aggregate is
  now maintained incrementally by the write paths that already compute the byte
  delta, and the sampler is a single relaxed load. The breaker's
  starvation-immunity property is unchanged: it still runs on a dedicated OS
  thread, it just no longer does O(indices × shards) work to do it.

- **A multi-index search runs its per-index searches concurrently**
  ([#875](https://github.com/xerj-org/xerj/issues/875)). The fan-out awaited each
  index before starting the next, so an `ax-*` query cost the SUM of its
  per-index latencies instead of the maximum — the shape every reference-coding
  retrieval uses. Hit ordering, aggregation merge order and the global search
  permit that bounds concurrency are all unchanged.

  Two behaviour changes worth knowing: `queries_by_index` now increments for
  every resolvable index even when a later index errors the request (those
  searches genuinely run now), and per-index deadlines all start at request time
  rather than sequentially, so a search queued behind the permit pool can return
  `timed_out: true` with partial results where the serial loop returned complete
  results slowly.

### Fixed

- **A field holding both numbers and strings in one segment no longer loses the
  numeric documents from `term` / `terms`.** The doc-values builder collects a
  segment's values into a numeric map (numbers and booleans) and a keyword map
  (strings), then writes both into one column set — so a *type-mixed* field had
  its numeric column silently overwritten by the keyword one, leaving every
  number/boolean document filed as null in a column the query prefilter treats
  as exact. Those documents were then dropped from the result with no error.
  Such a field now ships **no** doc-values column at all, exactly as a
  multi-valued field already does, and every consumer falls back to the
  stored-source scan: correct, at scan cost rather than column speed. Reindex
  onto a single type to get the column back. Whether it bit you depended on
  core count — a many-core host scatters a flush across shards and each segment
  stays single-typed, so this reproduced only where documents of both kinds
  landed in one segment.

- **A field mapped `integer`, `long`, `float` or `boolean` now enforces that
  type on write** ([#781](https://github.com/xerj-org/xerj/issues/781)). The
  declared type was enforced for nothing: `1.9` into an `integer` field was
  stored as `1.9`, `9999999999` was kept exact, `"abc"` was stored as a
  string, `"yes"` went into a `boolean`, and — per the issue's follow-up — an
  entire nested object `{"bad":"x"}` was indexed into an `integer`. Every one
  of those answered `201 created`.

  The consequence was wrong hits, not just leniency. With `1.9` sitting in an
  `integer` field, `range {"i":{"gte":1.5}}` **matched** in XERJ and would not
  in ES (which indexes the truncated `1`), while `term {"i":1}` matched in ES
  and returned nothing here — the same query, over the same document, under
  the same mapping, giving different answers.

  Ingest now applies ES 8.x's own rules (`coerce` defaults to `true`): `1.9`
  → `1`, `"5"` → `5`, `"1.9"` → `1`, `"true"` → `true`; out-of-range for the
  declared width, an unparseable string, an object, an array element that is
  none of those, and anything but `true`/`false` in a `boolean` are refused
  with a 400 `document_parsing_exception` — a per-item 400 under `_bulk`. An
  explicit `"coerce": false` additionally refuses a decimal part and a string,
  and `"ignore_malformed": true` still wins: that field's bad values are
  dropped into `_ignored` rather than failing the document.

  **What this changes about stored data, and what it means on upgrade.** A
  coerced value is rewritten in `_source`: ES keeps `_source` verbatim and
  coerces only the indexed value, but XERJ indexes from the stored source, so
  matching ES's *hits* costs source fidelity here. A document written
  `{"i": 1.9, "b": "false"}` now reads back `{"i": 1, "b": false}`. An index
  that spans this upgrade therefore holds **both** spellings of the same
  logical value — documents written before hold `"false"` / `"5"`, documents
  written after hold `false` / `5` — and no reindex is performed for you.

  That mixed index still answers correctly, and closing that gap is part of
  this change. Because the canonicalisation moves a declared `boolean` from
  the string spelling to a real JSON boolean in `_source`, the **query** side
  now runs `term` / `terms` values on a `boolean` field through the same
  predicate — ES parses a query value with the same `Booleans` its field
  mapper uses, so `"true"` and `true` name one term. Without it,
  `terms {"b":["true"]}` found nothing on a doc-values-only boolean field
  while the equivalent `term` still matched. The `_source` scan comparator
  relates the two spellings in both directions (as it already did for a number
  and its string spelling), for single- and multi-valued fields alike, and a
  `terms` aggregation buckets them together — so one query sees the whole
  index rather than half of it. **Reindex** if you want a uniform `_source`;
  nothing breaks if you do not.

  Also narrowed deliberately, all documented in `xerj_common::field_coercion`:
  the index-level `index.mapping.coerce` setting is not read (only the
  field-level parameter), `scaled_float` is range-checked but not quantised by
  its `scaling_factor`, an empty string is left alone rather than dropped, and
  `_update` is not re-validated — it merges into a document already checked on
  write.

- **A top-level `knn` beside a `query` now scores as a sum over the union of
  both halves, aggregations included**
  ([#825](https://github.com/xerj-org/xerj/issues/825)). ES 8.x defines this
  shape as a disjunction: the hit set is the union of the lexical matches and
  the global top-`k` neighbours, each document scores `query_score +
  knn_score`, and aggregations are computed over that union. XERJ folded the
  request to `bool.should[query, knn]` but the generic matcher/scorer has no
  `knn` arm, so a vector-only document vanished and a lexical match lost its
  vector score — silently, with `_shards.failed: 0`. The
  [#458](https://github.com/xerj-org/xerj/issues/458) carve-out covered one
  narrow shape with RRF rank fusion (itself a scoring divergence from ES) and
  left every request carrying `aggs`, `sort`, `collapse` or `rescore` on the
  silent drop. The engine now pre-executes the `knn` leg and pins its top-`k`
  into the tree as per-document constant scores, so one path serves the union
  with summed scores and every request feature applies to it.

  Two behaviour changes worth knowing before you upgrade:

  - `{query, knn:{field: <not a vector field>}, aggs}` now answers **400**
    (`the field is not answerable by knn`), matching ES and the existing
    [#498](https://github.com/xerj-org/xerj/issues/498) rule. It previously
    answered 200 with lexical-only hits — the wrong answer, quietly.
  - **This shape is slower.** A pinned tree contains `ids` clauses, which the
    FTS projection cannot lift, so the request is answered by a stored-document
    scan instead of the inverted index. Measured on 100 000 documents (`text` +
    8-dim `dense_vector`, ~10 % lexical selectivity, closed-loop, fresh query
    vector per request): `query` + `knn` k=10 goes from 2.5 ms (the old RRF
    route, which returned ES-divergent scores) to ~208 ms; k=1000 to ~385 ms;
    k=10000 to ~5.1 s. The shapes that previously carried the silent drop —
    anything with `aggs`/`sort`/`collapse`/`rescore` — already took the scan
    and cost about a quarter more (476 ms → 605 ms with a terms agg),
    but they now return the right documents. Restoring an indexed route is
    tracked as [#892](https://github.com/xerj-org/xerj/issues/892);
    correctness landed first.

- **`match_phrase` `slop` now admits transposed terms at Lucene's cost of 2**
  ([#830](https://github.com/xerj-org/xerj/issues/830)). The sloppy-phrase
  walk was in-order only — each next term matched strictly after the
  previous one — so a reordered pair never matched at ANY slop:
  `{"match_phrase":{"t":{"query":"quick brown","slop":2}}}` returned zero
  hits on a document reading `brown quick`, where Lucene/ES match it at
  distance 2 (`SloppyPhraseMatcher`'s own javadoc example). The evaluator is
  now the Lucene move-distance semantics — pick one document position per
  phrase term; the distance is the span of the positions after subtracting
  each term's query offset — implemented once
  (`xerj_fts::search::phrase_positions_match`) and called by both the segment
  positional clause and the engine's memtable/stored-scan walk
  (`phrase_walk`), so slop is evaluated identically on either side of a
  flush. `match_phrase_prefix` and `multi_match` phrase/phrase_prefix go
  through the same evaluator and gain the same behavior. Documents that
  matched before still match — an in-order pick has strictly increasing
  positions, so its span telescopes to exactly the old summed-gaps value —
  and that is checked rather than assumed, by an exhaustive test over every
  document of length <= 5 and every phrase of length <= 3 from a 3-symbol
  alphabet at slop 0..3, comparing against both the old walk and a
  brute-force reference. A repeated phrase term still needs as many DISTINCT
  document positions as it has slots (`"a a"` does not match a doc holding
  one `a`).


- **A small index reaches a segment instead of living in memory forever**
  ([#873](https://github.com/xerj-org/xerj/issues/873)). `needs_flush` was
  threshold-only, so a dataset below the size thresholds never flushed: its
  documents stayed memtable-resident for the life of the process, pinned their
  WAL generations on disk, and replayed on every start. Measured on a real
  corpus: a 100,001-document dataset with zero segments 30+ minutes after
  ingest. New `storage.flush_idle_secs` (default 300 s, `0` disables) flushes a
  non-empty memtable whose contents have not changed for that long, detected by
  a probe on the existing periodic flusher so the write path pays nothing. This
  matches Elasticsearch's `indices.memory.shard_inactive_time` default.

- **`match_phrase` `slop` admits transposed terms at Lucene's cost of 2**
  ([#830](https://github.com/xerj-org/xerj/issues/830)). The sloppy-phrase walk
  was in-order only — each next term had to match strictly after the previous —
  so `"quick brown"~2` never matched `brown quick`, diverging from Lucene's
  `SloppyPhraseMatcher`. It is now a minimal-window sweep over adjusted
  positions, shared by the memtable and segment arms so the answer does not
  change at flush, and covering `match_phrase`, `match_phrase_prefix` and
  `multi_match` phrase modes.

- **A date field's epoch scale comes from its mapping, not from each value**
  ([#790](https://github.com/xerj-org/xerj/issues/790)). The scale was guessed
  per value — four or more fractional-second digits meant nanoseconds, coarser
  meant milliseconds — so one column could hold both scales six orders of
  magnitude apart. A sub-millisecond document matched a `range` or `term` before
  flush and vanished after it, because the millisecond bound bisected to nothing
  against a nanosecond sort key. Precision is now a property of the field
  (`date` is milliseconds, a declared `date_nanos` is nanoseconds), threaded
  through every sort and shadow-range site. No on-disk format change.

- **Numeric and boolean fields enforce their declared type**
  ([#781](https://github.com/xerj-org/xerj/issues/781)). A field mapped
  `integer` accepted `1.9` and kept it, accepted values outside the type's
  range, and accepted non-numeric strings. Writes now follow ES's `coerce`
  semantics: `1.9` into an integer type stores `1`, `"5"` stores `5`, and
  out-of-range or unparseable values are refused with a 400 instead of being
  stored. A field-level `"coerce": false` is honoured, and `ignore_malformed`
  keeps precedence.

  **Upgrade note:** coercion rewrites the value in `_source`, so an index that
  spans this upgrade can hold both spellings of the same logical value. `term`
  and `terms` on a boolean field now normalise their operand the way `term`
  already did, and the two spellings compare equal, so a query matches
  documents written on either side of the upgrade.

- **`knn` beside `query` scores as a sum over the union**
  ([#825](https://github.com/xerj-org/xerj/issues/825)). The ES-native hybrid
  body silently dropped the vector half whenever aggregations, sort, collapse,
  rescore or highlight were present: the request answered 200 with a purely
  lexical result set and no warning. The vector leg is now pre-executed and its
  top-k pinned into the query tree, so the hit set is the union, a document
  reached by both halves scores the sum, and every request feature applies.

  **Known regression, measured and not hidden:** on the hybrid shape that
  previously took the RRF route, this is substantially slower — the pinned
  clauses cannot project to the full-text index, so the query falls back to a
  stored-document scan (~208 ms vs ~2.5 ms on a 100k-document index at k=10).
  Correct-and-slow was chosen over fast-and-wrong;
  [#892](https://github.com/xerj-org/xerj/issues/892) carries the indexed-route
  design that removes it.

- **`autoindex` never aborts a run over a legacy global-catalog mapping**
  ([#755](https://github.com/xerj-org/xerj/issues/755)). Upgrading onto a
  catalog whose `prefix` field pre-existed as `text` aborted the entire
  indexing run. An upgrade must never do that.

- **Removing a file no longer leaves graph edges pointing at it**
  ([#736](https://github.com/xerj-org/xerj/issues/736)). Deletion removed a
  file's documents and its outbound edges but left inbound edges dangling, so
  graph queries returned edges to nodes that no longer existed.

### Recorded late

These shipped in rc.70 or earlier and were never written down, because
rc.19–rc.70 have no CHANGELOG sections at all. They are recorded here rather
than left under an "Unreleased" heading that misdescribes them. *(The gap has
since been closed — the reconstructed rc.19–rc.70 sections appear below, so
these fixes now also appear in their true releases. The entries are kept here
because they carry the fuller write-up.)*

- **Wrapping a query in a one-clause `bool` no longer changes its `_score` or
  its ranking** ([#399](https://github.com/xerj-org/xerj/issues/399)).
  `{"bool":{"must":[X]}}` and bare `X` are the same query — Lucene's
  `BooleanQuery.rewrite` erases a one-clause boolean before any `Weight` is
  built (`BooleanQuery.java:279-298`) — but XERJ steers on the *shape* of the
  query tree, and `is_doc_scan_query` matches every `bool`. On an index whose
  documents were still in the memtable, the wrapped form was therefore scored
  per-document by the IDF-less brute scorer (`1 + ln(1 + tf)`) while the
  identical bare `match` took the memtable BM25 arm. Measured on a
  120-document fixture with one `text` field, same clause throughout: the
  wrapped query scored `d004` at `2.7917595` where the bare clause scored it
  `0.0068451734`, and `size:1` returned a different document for each. The
  flushed (segment FTS) population already agreed and still does.

  A one-clause `bool` is now erased before any path decision reads the tree —
  a lone `must`, or a lone `should` with `minimum_should_match` absent, `0`
  or `1`, exactly the cases Lucene rewrites to the clause itself. A lone
  `filter` (`BoostQuery(ConstantScoreQuery(q), 0)`, score 0) and a lone
  `must_not` (pure-negative) are *not* their child and are left alone. The
  two page-local score-rewrite gates further down now read the same
  normalised tree the executor was steered by, rather than the raw request,
  so the divergence cannot reappear one gate later.

  Known and unchanged: on an unflushed index a `filter`-only `bool` still
  scores `1.0` instead of `0.0`. That is the separately-tracked filter-context
  defect in the memtable's brute scorer, not this one, and this change neither
  fixes nor worsens it.

- **A file excluded by a widened ignore/hidden rule was reported as a user
  deletion, so the graph resume path refused the rerun and its recovery
  contract told operators to "restore the removed file(s)" — a file still on
  disk** ([#439](https://github.com/xerj-org/xerj/issues/439), first fix). The
  resume-plan reconciler (`UnsupportedInventoryDelta::between`) now splits a plan
  file that vanished from the walk by whether it is still on disk, probed through
  the reversible raw-bytes `path_id` so a hidden *non-UTF-8* name (the case #439
  was filed for) is matched instead of read as a phantom removal through its
  lossy `rel`. A genuine deletion keeps the "restore the removed file(s)" route;
  a still-on-disk exclusion now says the file is still on disk and names the real
  routes (narrow the rule to re-admit it, or rebuild isolated to drop it). The
  refusal itself is unchanged — the documents still stay live — because sweeping
  an excluded group from the destination (so a widened exclusion actually removes
  the data) is a larger change tracked in
  [#589](https://github.com/xerj-org/xerj/issues/589).

- **`sort` on a field XERJ cannot resolve returned `null` on every hit
  instead of an error** ([#437](https://github.com/xerj-org/xerj/issues/437)).
  `compute_sort_values` special-cased `_score`, `_doc`, `_id` and the four
  meta-fields [#420](https://github.com/xerj-org/xerj/pull/420) made
  resolvable (`_seq_no`, `_version`, `_primary_term`, `_index`); every other
  ES meta-field name (`_source`, `_size`, `_doc_count`, `_field_names`,
  `_meta`, `_tier`, `_nested`, `_nested_path`, `_feature`, `_parent`,
  `_matched_queries`) and any unmapped or misspelled field fell through to a
  plain `_source` lookup that returned `Null` when absent. The whole result
  set tied on `[null]`, HTTP 200, and `search_after` paging on that field
  was stranded at page one with no error anywhere — the same
  silently-truncated-export failure mode
  [#198](https://github.com/xerj-org/xerj/issues/198) closes for scroll,
  reached through `sort` instead.

  A prior attempt, [#402](https://github.com/xerj-org/xerj/pull/402), tried
  a request-level denylist of "known-unresolvable" field names and was
  closed after independent verification refuted it: XERJ deliberately
  accepts a metadata-named key inside a document body (`{"_seq_no": 1}`,
  where real Elasticsearch rejects it — see #420's entry above), so whether
  a name resolves is a property of the *corpus* being searched, not of the
  field name, and a static list is guaranteed wrong on some corpus. #402
  was also wired into `_search` only, leaving `_msearch`,
  `_search/template` and `_async_search` answering the identical
  "unresolvable" field with 200.

  This fix checks the index's **schema** instead of the corpus — the same
  thing real Elasticsearch validates — with a single gate inside the
  engine's `search_inner`, which every search entry point funnels through
  regardless of endpoint. A sort field that is not one of the seven
  already-resolvable special cases and is not declared in the schema (via
  the same recursive dotted-path resolution already used for `.keyword`
  multi-fields and nested/object properties) is now rejected up front with
  Elasticsearch's real error: a 400 `search_phase_execution_exception`
  wrapping `query_shard_exception`, reason `"No mapping found for [x] in
  order to sort on"`.

  **Known gap, not closed here:** `collapse.inner_hits.sort` and
  aggregation `top_hits.sort` implement their own sorting independently of
  `compute_sort_values` and are not covered by this gate — an unresolvable
  field named in either still sorts silently wrong rather than erroring.
  Tracked as a follow-up.

## [1.0.0-rc.71] - 2026-08-29

An Elasticsearch-compatibility correctness release: query, aggregation, API and
autoindex semantics defects, a ~100x incremental re-index, and two metrics
counters that had been registered but never incremented.

Note on scope: releases rc.19 through rc.70 shipped without CHANGELOG sections.
This entry covers only rc.70..rc.71 and does not attempt to reconstruct that
gap. The three entries still under `[Unreleased]` above describe work that
shipped in rc.70 or earlier and are deliberately left there rather than
silently reattributed.

### Fixed — query semantics

- **`match` honors `minimum_should_match`** ([#832](https://github.com/xerj-org/xerj/issues/832)).
  It was ignored outright, so every `match` behaved as a pure OR.
- **`minimum_should_match` honors negative and combination specs**
  ([#846](https://github.com/xerj-org/xerj/issues/846)). `-2`, `-25%` and
  `"3<90%"` now resolve against the should-clause count instead of degrading to
  OR. The `MinShouldMatch` `Serialize` impl is now hand-written: an untagged
  derive emitted `Negative(2)`, `Percentage(2)` and `Fixed(2)` all as the bare
  number `2`, so the query cache — which hashes the serialized request —
  conflated three semantically different queries into one entry
  ([#860](https://github.com/xerj-org/xerj/issues/860)).
- **`dis_max` over memtable documents scores through the real per-field BM25
  path** ([#834](https://github.com/xerj-org/xerj/issues/834)). Buffered matches
  took a flat-1.0 catch-all, so a document matching more disjuncts tied with
  single-disjunct documents and the ranking changed at flush.
- **A `nested` query scores parents by the child `score_mode`**
  ([#836](https://github.com/xerj-org/xerj/issues/836)) — `avg` (the ES default),
  `max`, `min`, `sum`, `none` — instead of contributing a flat 1.0.
- **`fuzzy` and `match` honor `prefix_length`**
  ([#848](https://github.com/xerj-org/xerj/issues/848)), on the doc-scan and
  segment paths and through nested-path rewriting.
- **A `must_not`-only `bool` no longer projects to a pure-negative FTS bool**
  ([#797](https://github.com/xerj-org/xerj/issues/797)).
- **A `term` on a date field matches by instant across every path**
  ([#788](https://github.com/xerj-org/xerj/issues/788)), and `terms` on date/ip
  fields normalizes values the same way a single `term` does
  ([#799](https://github.com/xerj-org/xerj/issues/799),
  [#801](https://github.com/xerj-org/xerj/issues/801)).
- **`exists` requires a non-null value, not merely key presence**
  ([#792](https://github.com/xerj-org/xerj/issues/792)).
- **A keyword `prefix`/`wildcard` inside `bool.must_not` stays case-sensitive
  after flush** ([#794](https://github.com/xerj-org/xerj/issues/794)).
- **A CIDR `term` survives flush** — rewritten to a range on an `ip` field
  ([#782](https://github.com/xerj-org/xerj/issues/782)) and routed to the source
  scan when schemaless ([#786](https://github.com/xerj-org/xerj/issues/786)).
- **`function_score` per-function filters are schema-aware**
  ([#802](https://github.com/xerj-org/xerj/issues/802)) — the last schemaless
  `doc_matches_query` caller in production code.
- **`sort` on a multi-valued field reduces to a min/max representative**
  ([#826](https://github.com/xerj-org/xerj/issues/826)).

### Fixed — aggregations

- **Metric aggregations fold ALL values of a multi-valued field**
  ([#842](https://github.com/xerj-org/xerj/issues/842)), instead of the first.
- **`terms` `include`/`exclude` regexes are fully anchored**
  ([#837](https://github.com/xerj-org/xerj/issues/837)) — they matched as
  substrings.
- **`terms` bucket keys are typed by the field's source shape, not by content**
  ([#864](https://github.com/xerj-org/xerj/issues/864)). A keyword value that
  merely looks numeric (`"007"`) stayed a string instead of being coerced to 7.
- **`terms` `_key` ordering is numeric on numeric fields**
  ([#839](https://github.com/xerj-org/xerj/issues/839)).
- **`missing` counts empty and only-null arrays as missing**
  ([#844](https://github.com/xerj-org/xerj/issues/844)) and resolves presence
  via the `exists` resolver ([#847](https://github.com/xerj-org/xerj/issues/847)).
- **`filters` supports `other_bucket` / `other_bucket_key`**
  ([#856](https://github.com/xerj-org/xerj/issues/856)).

### Fixed — API

- **`mget` honors a per-item `_source`**
  ([#855](https://github.com/xerj-org/xerj/issues/855)) — `false`, an includes
  list, or `{"includes":[..],"excludes":[..]}`.
- **`GET /_doc` `_source_includes`/`_source_excludes` honor nested paths**
  ([#850](https://github.com/xerj-org/xerj/issues/850)).
- **`_field_caps` `fields` filtering honors full globs**
  ([#853](https://github.com/xerj-org/xerj/issues/853)).
- **A partial `_update` deep-merges nested objects**
  ([#821](https://github.com/xerj-org/xerj/issues/821)) — it was shallow and
  dropped sibling keys.

### Fixed — autoindex

- **C++ symbols defined inside a function body are no longer indexed**
  ([#852](https://github.com/xerj-org/xerj/issues/852)) — a function-local
  `struct`/`class` and its enumerators leaked as file-scope API symbols.
- **Top-level constants are indexed as symbols across 7 languages**
  ([#500](https://github.com/xerj-org/xerj/issues/500)).

### Fixed — observability

- **`xerj_bytes_written_total` is wired to the flush and merge segment writes**
  ([#804](https://github.com/xerj-org/xerj/issues/804)) and
  **`xerj_bytes_read_total` to validated segment opens**
  ([#819](https://github.com/xerj-org/xerj/issues/819)). Both counters were
  registered but never incremented, so each read a flat zero regardless of load.
- **The disk flood-stage log reports free space, and the watermark can be
  retuned at runtime** ([#823](https://github.com/xerj-org/xerj/issues/823))
  via `PUT _cluster/settings`, matching ES's recovery flow without a restart. An
  override cannot enable the watermark when it is disabled in config.

### Performance

- **Incremental re-index skips the Phase-A parse for byte-identical files**
  ([#868](https://github.com/xerj-org/xerj/issues/868)). The tree-sitter parse
  is ~72% of extraction cost (~57 ns/byte, measured), and an unchanged re-index
  re-parsed the whole corpus to rebuild sketches. Gating on the committed
  content digest turns that from O(corpus) into O(changed) — roughly 100x on the
  edit-and-rerun and CI paths. The digest check in `reconcile_plan` remains the
  correctness authority: a changed file whose scan was wrongly skipped
  mismatches there and fails closed rather than carrying stale documents.

  Known limitation: the fast path is keyed on file content only. It carries no
  extractor version, so upgrading to a release that extracts *more* symbols does
  not re-extract unchanged files until their content changes.

### Changed

- **Release PRs are gated on their notes matching the tree**
  ([#474](https://github.com/xerj-org/xerj/issues/474)) — issue states against
  the live tracker, merge coverage since the previous tag, and freshness against
  the base.
- **The Build+Test job timeout is 60 minutes** (was 50)
  ([#770](https://github.com/xerj-org/xerj/issues/770)). This is defense in
  depth, not a fix: [#751](https://github.com/xerj-org/xerj/issues/751), the
  intermittent hang in the default-parallelism workspace test step, is still
  open and still bounded by that step's own 12-minute cap.
- **`llms.txt` no longer hardcodes the current release version**
  ([#796](https://github.com/xerj-org/xerj/pull/796)) — it drifted behind every
  release and handed agents the wrong version.

### Documentation

- The reference-coding case study's charts are rebuilt as vertical comparison
  bars, and its colours now resolve in both themes (the page referenced CSS
  variables that do not exist, so every chart fell back to night-only values).
- `match_phrase` `slop` is documented as in-order only, not transpositions
  ([#830](https://github.com/xerj-org/xerj/issues/830)).

> **Reconstructed 2026-09-26.** Releases rc.19–rc.70 shipped without CHANGELOG
> sections at cut time (acknowledged in the rc.71 note below). The entries in that
> range were rebuilt after the fact from `git log` (merge subjects and commit
> bodies) and each tag's GitHub release record; every issue/PR link they carry was
> checked to exist within its release window. They are drier than ship-time entries —
> numbers are quoted only where a commit body recorded them — and nothing was
> force-fitted: the three entries that were still under `[Unreleased]` when rc.71
> shipped were recorded, fuller, under rc.72's "Recorded late" heading above, and
> stay there.

## [1.0.0-rc.70] - 2026-08-26

A CLI and autoindex-ergonomics release: `xerj search` lands as the one-command
query client, autoindex warnings stop being silencible, and engine invariants
for #421 get binding tests.

### Added

- **`xerj search` — the binary as a query client** ([#764](https://github.com/xerj-org/xerj/pull/764)).
  Query from the CLI directly; the reference-coding demo drops its scripts.
  Agent docs now teach it as the one-command query ([#771](https://github.com/xerj-org/xerj/pull/771)),
  and it renders a code-symbol line plus code fields.

### Fixed

- **XERJ_URL warnings survive `--quiet`** ([#768](https://github.com/xerj-org/xerj/issues/768), via
  [#769](https://github.com/xerj-org/xerj/pull/769)). The wrong-node warning is routed through
  `pr.warn` instead of a bare `eprintln`, so it can no longer be silenced. Companion fix
  [#765](https://github.com/xerj-org/xerj/issues/765) ([#766](https://github.com/xerj-org/xerj/pull/766)):
  warn when XERJ_URL is set but `--url` was not passed, extended to map and status.
- **Truncated files named in the #381 cap note** ([#759](https://github.com/xerj-org/xerj/issues/759),
  via [#762](https://github.com/xerj-org/xerj/pull/762)); the #381 cap is also pinned at its exact boundary.
- **`semantic_text` hint suppressed only when the vector dispatches** ([#394](https://github.com/xerj-org/xerj/issues/394),
  via [#776](https://github.com/xerj-org/xerj/pull/776)), and the API docs stop over-claiming that
  `dispatching_vector_fields` mirrors peel exactly ([#777](https://github.com/xerj-org/xerj/issues/777),
  [#778](https://github.com/xerj-org/xerj/pull/778)).
- **Legacy file-alias catalog doc swept by id** ([#739](https://github.com/xerj-org/xerj/issues/739),
  via [#754](https://github.com/xerj-org/xerj/pull/754)); the #729/#730 review nits documented under
  [#731](https://github.com/xerj-org/xerj/issues/731) ([#753](https://github.com/xerj-org/xerj/pull/753)).

### Changed

- **CI Build+Test hangs bounded** ([#751](https://github.com/xerj-org/xerj/issues/751), via
  [#767](https://github.com/xerj-org/xerj/pull/767)): step-6 plus a job-level timeout so main is
  not stuck for hours.

### Changed — tests / docs

- **#421 invariants bound**: the `_id` sort-value type and the
  `build_doc_value_columns` underscore-skip invariant each get tests
  ([#772](https://github.com/xerj-org/xerj/pull/772), [#773](https://github.com/xerj-org/xerj/pull/773)).
- **#755 catalog-error enrichment wiring guarded** ([#760](https://github.com/xerj-org/xerj/pull/760),
  via [#761](https://github.com/xerj-org/xerj/pull/761)).
- **Six-model first-contact field report** ([#774](https://github.com/xerj-org/xerj/pull/774)):
  XERJ works; agents skip the required report. In response, llms.txt surfaces
  `xerj feedback` as first-run step 4 ([#775](https://github.com/xerj-org/xerj/pull/775)).
- Release chores: landing version stamps bumped to rc.69 ([#763](https://github.com/xerj-org/xerj/pull/763)).

## [1.0.0-rc.69] - 2026-08-25

An autoindex-focused correctness release: two defect fixes in file
section-record handling and legacy-catalog errors, plus landing version
chores.

### Fixed — autoindex

- **Section records per file are capped and truncation is flagged**
  ([#381](https://github.com/xerj-org/xerj/issues/381),
  [#758](https://github.com/xerj-org/xerj/pull/758)). The cap is applied on
  both the graph index path and the `--no-graph` generated path, and
  truncation is surfaced rather than silently dropped.

- **Legacy-catalog mapping conflicts return an explanation instead of an
  opaque 400** ([#755](https://github.com/xerj-org/xerj/issues/755),
  [#756](https://github.com/xerj-org/xerj/pull/756)).

### Changed — chores

- release chores: landing version stamps bumped to the released rc.68, with
  SEO heads and sitemap regenerated
  ([#757](https://github.com/xerj-org/xerj/pull/757)).

## [1.0.0-rc.68] - 2026-08-25

An autoindex-correctness release: the exclusion sweep gained the missing
invalidations and scoping it needed, plus a batch of --no-graph pipeline fixes;
around it, an embedder identity fix, a self-verifying release/installer
toolchain, an agentic-SEO content programme, and CI hardening.

### Fixed — autoindex

- **Exclusion sweeps now purge everything a removed file leaves behind** — graph
  edges ([#694](https://github.com/xerj-org/xerj/issues/694)), alias catalog
  docs ([#693](https://github.com/xerj-org/xerj/issues/693)), inbound graph edges
  ([#736](https://github.com/xerj-org/xerj/issues/736) sweep half), and a legacy
  `file:` catalog doc by id ([#739](https://github.com/xerj-org/xerj/issues/739)).
- **Sweeps are prefix-scoped** so a sibling corpus is not over-deleted
  ([#737](https://github.com/xerj-org/xerj/issues/737)).
- **`--no-graph` pipeline fixes**: the generated pipeline never consumed
  `as_document` ([#722](https://github.com/xerj-org/xerj/issues/722)); a new
  document/config is routed to the docs dataset on incremental runs, and
  acceptance no longer re-counts pruned fields
  ([#729](https://github.com/xerj-org/xerj/issues/729)); a `--no-graph` genesis
  bootstrap continued on the graph path is now refused
  ([#585](https://github.com/xerj-org/xerj/issues/585) case 2).
- **A byte-identical duplicate no longer wedges a completed run**
  ([#345](https://github.com/xerj-org/xerj/issues/345)).
- **Catalog read-back diagnostics name the disagreeing field** (#367), with the
  diff truncated on a char boundary.
- **Files skipped after plan freeze surface distinctly**, not below the noise
  floor ([#346](https://github.com/xerj-org/xerj/issues/346)); the genesis-guard
  recovery message offers `--fresh` and the dry-run refusal is documented
  ([#718](https://github.com/xerj-org/xerj/issues/718)); a frontmatter body is
  classified by its structured family (#587 item 1).

### Fixed — engine

- **The proxy endpoint is folded into the embedder identity** (#533); the wire
  identity version stays at 1 with a separate on-disk record version, and the
  version-delta grandfather still refuses a backend change.

### Fixed — release & site

- **Correct CLI banner version, /get.sh installer, self-updating RC number**
  ([#735](https://github.com/xerj-org/xerj/pull/735)), hotfixed for three
  defects that merge introduced
  ([#741](https://github.com/xerj-org/xerj/pull/741)), with a /get.sh fallback
  when Functions are off ([#740](https://github.com/xerj-org/xerj/issues/740)).

### Added

- **Agentic-SEO programme** ([#550](https://github.com/xerj-org/xerj/pull/550)):
  76 articles, 16 comparisons, plus site-wide SEO repair.
- **grep-vs-retrieval benchmark harness for Lucene**
  ([#493](https://github.com/xerj-org/xerj/pull/493)) — harness only, no
  results yet.

### Changed — ci / tests / chores

- CI merges resolve before landing, tree-sitter hoisted
  ([#352](https://github.com/xerj-org/xerj/issues/352)); every workflow action
  pinned to a commit SHA and the toolchain to 1.97.1
  ([#456](https://github.com/xerj-org/xerj/issues/456)); the default-parallelism
  gate widened to the full `--workspace` (#384); landing/ deploys to Cloudflare
  Pages. Test coverage for the Percentage/Field/Script msm unwrap (#643 gap 1).
  Release chores: version stamps, CLA + artifact-verification reports from a
  new contributor, stale-comment corrections (#719, #725).

## [1.0.0-rc.67] - 2026-08-24

A small correctness-and-docs release: one engine fix to dynamic field-budget
admission counting, two test-docstring corrections from review follow-ups, and
the rc.66 landing stamp.

### Fixed

- **Dynamic field-budget counts the whole subtree at admission**
  ([#312](https://github.com/xerj-org/xerj/issues/312),
  [#708](https://github.com/xerj-org/xerj/pull/708)). The admission-time budget
  now accounts for the entire field subtree rather than a partial count.

### Docs

- **Corrected the #643 neutrality-guard docstring** (review follow-up)
  ([#643](https://github.com/xerj-org/xerj/issues/643),
  [#704](https://github.com/xerj-org/xerj/issues/704),
  [#706](https://github.com/xerj-org/xerj/pull/706)).
- **Corrected the #577 `residual_gate_bounds` `materialisation_limit` comment**
  ([#577](https://github.com/xerj-org/xerj/issues/577),
  [#705](https://github.com/xerj-org/xerj/issues/705),
  [#709](https://github.com/xerj-org/xerj/pull/709)).

Release chores: landing version stamp for v1.0.0-rc.66
([#707](https://github.com/xerj-org/xerj/pull/707)).

## [1.0.0-rc.66] - 2026-08-24

An engine-correctness release: three query-semantics and kNN filtering fixes,
plus the rc.65 landing stamp. The metrics chore logged 1928 assets and 1013
binaries downloaded as of cut day.

### Fixed

- **`residual_gate` eager-trims `all_hits` to the page cap** ([#577](https://github.com/xerj-org/xerj/issues/577),
  [#705](https://github.com/xerj-org/xerj/pull/705)). The gate now trims the
  hit set to the requested page size instead of carrying the full set.

- **Single-clause `bool` is unwrapped through `function_score` and `dis_max`**
  ([#643](https://github.com/xerj-org/xerj/issues/643),
  [#704](https://github.com/xerj-org/xerj/pull/704)). Wrappers no longer hide
  a one-clause bool from scoring semantics.

- **Nested kNN pre-filter evaluates schema-aware** ([#423](https://github.com/xerj-org/xerj/issues/423),
  [#702](https://github.com/xerj-org/xerj/pull/702)). Pre-filtering on nested
  documents now respects the index schema; the direct commit extends the same
  treatment to post-filters.

### Changed — chores

- Release chores: landing stamp for v1.0.0-rc.65
  ([#703](https://github.com/xerj-org/xerj/pull/703)).

## [1.0.0-rc.65] - 2026-08-24

A small correctness release: one engine fix to filtered kNN, plus docs and
release-chores merges.

### Fixed

- **Filtered kNN evaluates its filter schema-aware**
  ([#423](https://github.com/xerj-org/xerj/issues/423),
  [#701](https://github.com/xerj-org/xerj/pull/701)). The filter used during
  kNN search is now evaluated with awareness of the schema.

### Changed — docs & chores

- **Softened `meta_sort_value` docs claim** ([#421](https://github.com/xerj-org/xerj/issues/421),
  [#699](https://github.com/xerj-org/xerj/pull/699)) — item 2 of #421: the
  sort-vs-`_seq_no` agreement claim was toned down.
- Release chores: landing stamp for v1.0.0-rc.64
  ([#700](https://github.com/xerj-org/xerj/pull/700)).

## [1.0.0-rc.64] - 2026-08-23

A two-fix engine release: a memtable sort-path performance defect and a
nested-terms aggregation correctness defect, plus release chores.

### Fixed

- **Meta-aware memtable pre-clone rejection for `_seq_no`/`_version` sorts**
  ([#421](https://github.com/xerj-org/xerj/issues/421),
  [#698](https://github.com/xerj-org/xerj/pull/698)). The memtable was
  pre-cloned on sorts over `_seq_no`/`_version`; the clone is now rejected for
  those meta fields, restoring O(1) behavior.

- **Nested-terms sub-aggregations deferred to surviving top-N buckets**
  ([#375](https://github.com/xerj-org/xerj/issues/375),
  [#696](https://github.com/xerj-org/xerj/pull/696)). Sub-aggregations under
  nested `terms` now run only over the buckets that survive top-N truncation
  rather than over all buckets.

### Changed — chores

- Release chores: landing the v1.0.0-rc.63 version stamp
  ([#697](https://github.com/xerj-org/xerj/pull/697)).

## [1.0.0-rc.63] - 2026-08-23

An autoindex-semantics release: two fixes around excluded groups and pending
no-graph generations, one test extraction, and a version stamp.

### Fixed — autoindex

- **A newly-excluded group's documents are swept, not refused** ([#589](https://github.com/xerj-org/xerj/issues/589)).
  When a group became excluded, the delta was rejected outright; it is now swept,
  with exact-dataset indices covered by a mock-ES integration test
  ([#690](https://github.com/xerj-org/xerj/pull/690)).
- **A pending `--no-graph` generation resumed on the graph path is refused**
  ([#585](https://github.com/xerj-org/xerj/issues/585)). "Case 1" of the
  graph-crosspath defect: a pending sync resumed under a mismatched graph mode
  is now rejected rather than silently continuing ([#692](https://github.com/xerj-org/xerj/pull/692)).

### Changed — tests

- **Extracted `tcorr_id` + prefix-scope unit test** ([#689](https://github.com/xerj-org/xerj/issues/689),
  [#695](https://github.com/xerj-org/xerj/pull/695)).

Release chores: landing the v1.0.0-rc.62 version stamp ([#691](https://github.com/xerj-org/xerj/pull/691)).

## [1.0.0-rc.62] - 2026-08-23

A small autoindex correctness release: one fix to correlation catalog id
scoping, plus the rc.61 landing stamp.

### Fixed

- **Prefix-scope for autoindex correlation catalog ids** ([#673](https://github.com/xerj-org/xerj/issues/673),
  [#688](https://github.com/xerj-org/xerj/pull/688)). Correlation catalog ids
  (`corr:`/`tcorr:`) are now scoped by prefix, per the fix in #688.

Release chores: landing the v1.0.0-rc.61 version stamp
([#687](https://github.com/xerj-org/xerj/pull/687)).

## [1.0.0-rc.61] - 2026-08-23

A single-fix correctness release plus release chores.

### Fixed

- **Single-clause bool unwrap now covers percentage `minimum_should_match`** ([#643](https://github.com/xerj-org/xerj/issues/643), [#686](https://github.com/xerj-org/xerj/pull/686)). The unwrap of single-clause bool queries handled integer-valued `minimum_should_match` but not percentage values below 200%.

Release chores: landing the v1.0.0-rc.60 version stamp ([#685](https://github.com/xerj-org/xerj/pull/685)).

## [1.0.0-rc.60] - 2026-08-23

A small query-semantics release: `_name` on bool and prefix/wildcard clauses,
plus the rc.59 landing stamp.

### Fixed — query semantics

- **`_name` honored on a bool clause** ([#681](https://github.com/xerj-org/xerj/issues/681),
  [#684](https://github.com/xerj-org/xerj/pull/684)). Named-query matching
  previously ignored `_name` set on a bool clause.
- **`_name` honored on the expanded prefix/wildcard form** ([#681](https://github.com/xerj-org/xerj/issues/681),
  [#682](https://github.com/xerj-org/xerj/pull/682)). The internally expanded
  prefix/wildcard rewrite dropped the clause name; #684 completes this fix for
  the bool form.

### Changed — chores

- release chores: landing version stamp for v1.0.0-rc.59
  ([#683](https://github.com/xerj-org/xerj/pull/683)).

## [1.0.0-rc.59] - 2026-08-23

An Elasticsearch-compatibility correctness release: two query-semantics fixes
(matched query naming and object sub-field term matching), plus a version
stamp. No performance numbers were cited at cut time.

### Fixed — query semantics

- **`matched_queries` (`_name`) uses the schema-aware matcher**
  ([#669](https://github.com/xerj-org/xerj/issues/669),
  [#680](https://github.com/xerj-org/xerj/pull/680)). Named-query reporting
  now goes through the same schema-aware matching path as the rest of the
  engine.

- **`term` on an object sub-field is flush-invariant**
  ([#677](https://github.com/xerj-org/xerj/issues/677),
  [#678](https://github.com/xerj-org/xerj/pull/678)). A term (and range count)
  on a dotted object sub-field could diverge depending on flush state; the
  matcher now falls back to a dotted-path source scan, with a reproducing
  test (previously ignored) landing alongside.

### Changed — chores

- Release chores: landing the v1.0.0-rc.58 version stamp
  ([#679](https://github.com/xerj-org/xerj/pull/679)).

## [1.0.0-rc.58] - 2026-08-23

A small correctness release: two search-semantics fixes (keyword fuzzy
case-sensitivity, object-field prefix/wildcard flush-invariance) plus a landing
version stamp.

### Fixed

- **Keyword fuzzy is case-sensitive by default** ([#423](https://github.com/xerj-org/xerj/issues/423),
  [#406](https://github.com/xerj-org/xerj/issues/406); PR
  [#675](https://github.com/xerj-org/xerj/pull/675)). Fuzzy previously ignored
  `case_insensitive` and always folded case; a `case_insensitive` flag is now
  threaded through `QueryNode::Fuzzy`, honored on all matcher paths, and the
  sub-token fallback on the DV path is gated behind it. Includes a
  fail-before repro test.

- **Object-field prefix/wildcard is flush-invariant** ([#413](https://github.com/xerj-org/xerj/issues/413);
  PR [#676](https://github.com/xerj-org/xerj/pull/676)). The memtable
  prefix/wildcard path now mirrors the whole-object blob, so results match
  across flushes.

- Release chores: landing version stamps, changelog housekeeping
  (rc.56 → rc.57, PR [#674](https://github.com/xerj-org/xerj/pull/674)).

## [1.0.0-rc.57] - 2026-08-23

A small correctness release: two engine/autoindex semantic fixes plus a release stamp.

### Fixed

- **Keyword wildcard matching is case-sensitive by default** ([#668](https://github.com/xerj-org/xerj/issues/668)).
  Previously the keyword wildcard path folded case; it now honors the Elasticsearch
  default, with a `case_insensitive` flag threaded through `QueryNode::Wildcard`
  so all matcher paths respect it. Landed with a fail-before repro test.

- **Autoindex catalog doc IDs are scoped by corpus prefix** ([#416](https://github.com/xerj-org/xerj/issues/416)).
  Catalog documents from different corpora no longer overwrite each other's IDs
  (no cross-corpus overwrite).

### Changed — chores

- release chores: landing version stamp-bump rc.55 → rc.56.

## [1.0.0-rc.56] - 2026-08-23

A single-fix correctness release: prefix and wildcard case-folding was
flush-invariant, plus the usual release stamp.

### Fixed

- **Prefix & wildcard case-folding is flush-invariant** ([#396](https://github.com/xerj-org/xerj/issues/396),
  [#667](https://github.com/xerj-org/xerj/pull/667)). A prefix query on a
  keyword field folded case while the document lived in the memtable but was
  case-sensitive once flushed to a segment, so the same query returned
  different results depending on flush state. Landed with fail-before and
  broadened repros (prefix also folded the query for text fields).

Release chores: landing version stamp rc.54 → rc.55
([#666](https://github.com/xerj-org/xerj/pull/666)).

## [1.0.0-rc.55] - 2026-08-23

A one-fix release: a count-semantics defect for `term` queries over keyword
arrays, plus the routine landing stamp.

### Fixed

- **`term` on a keyword array is flush-invariant for counts** ([#408](https://github.com/xerj-org/xerj/issues/408),
  [#665](https://github.com/xerj-org/xerj/pull/665)). Document counts for a
  `term` match against a keyword array no longer shift across segment flushes.

Release chores: landing version stamp rc.53 → rc.54
([#664](https://github.com/xerj-org/xerj/pull/664)).

## [1.0.0-rc.54] - 2026-08-23

A one-fix release: `POST /<index>/_close` now actually reclaims RAM instead of just flipping a flag, plus the routine version stamp.

### Fixed

- **`_close` frees memory; `_open` reconstructs the index; closed indices stay visible** ([#463](https://github.com/xerj-org/xerj/issues/463), [#663](https://github.com/xerj-org/xerj/pull/663)).
  The engine gained `close_index`/`reopen_index` methods that release caches while keeping the index on disk loadable, the API `_close`/`_open` endpoints were wired to them, and `_cat/indices` now lists closed indices with status `close` instead of dropping them from view.

Release chores: landing version stamp rc.52 → rc.53 ([#662](https://github.com/xerj-org/xerj/pull/662)).

## [1.0.0-rc.53] - 2026-08-23

A small API-consistency release: one scroll behavior fix plus a release stamp.

### Fixed

- **`_search_scroll` first page now suppresses `_source` like `search_impl`** ([#659](https://github.com/xerj-org/xerj/issues/659),
  [#661](https://github.com/xerj-org/xerj/pull/661)). The scroll API's first
  page diverged from the search path's `_source` handling; it now matches, with
  accompanying comment refreshes in the API docs.

### Changed — chores

- release chores: landing version stamp rc.51 → rc.52
  ([#660](https://github.com/xerj-org/xerj/pull/660)).

## [1.0.0-rc.52] - 2026-08-23

A small API/engine correctness release: two `_source` handling defects —
one in `_source` filtering inside nested arrays of objects, one in scroll
continuation — plus a release stamp chore.

### Fixed

- **`_source` filters project inside nested arrays of objects**
  ([#653](https://github.com/xerj-org/xerj/issues/653),
  [#656](https://github.com/xerj-org/xerj/pull/656)). Field-level `_source`
  projection had not been applied to hits whose values were nested arrays of
  objects.

- **Scroll continuation matches first-page `_source` suppression**
  ([#637](https://github.com/xerj-org/xerj/issues/637),
  [#658](https://github.com/xerj-org/xerj/pull/658)). Continuation pages
  ignored the `_source` suppression in effect on the opening request; the fix
  applies per-hit mapping suppression scoped to the opening route.

### Changed — chores

- release chores: landing stamp-bump v1.0.0-rc.50 → rc.51
  ([#657](https://github.com/xerj-org/xerj/pull/657)).

## [1.0.0-rc.51] - 2026-08-23

A single-fix correctness release: collapse results under an explicit `_source`
projection, plus the rc.50 landing stamp.

### Fixed

- **Collapse sentinels survive an explicit `_source` projection**
  ([#651](https://github.com/xerj-org/xerj/issues/651),
  [#655](https://github.com/xerj-org/xerj/pull/655)). Inner hits under
  `collapse` vanished when the request also carried `_source.includes`; the
  engine now preserves the collapse sentinels across that projection. Landed
  with a reproducing API test and a rustfmt pass.

Release chores: the v1.0.0-rc.49 → rc.50 landing stamp
([#654](https://github.com/xerj-org/xerj/pull/654)).

## [1.0.0-rc.50] - 2026-08-23

A small `_source` projection correctness release: two fixes around how source
filters apply inside arrays of objects, plus release chores.

### Fixed

- **`_source` filters are projected inside arrays of objects**
  ([#644](https://github.com/xerj-org/xerj/issues/644),
  [#652](https://github.com/xerj-org/xerj/pull/652)). Field-level source
  filtering previously did not descend into arrays of objects; emptied array
  elements and the empty array key are now pruned from the projection.

- **`inner_hits` members honour the default `_source` projection**
  ([#646](https://github.com/xerj-org/xerj/issues/646),
  [#650](https://github.com/xerj-org/xerj/pull/650)). Collapsed inner_hits
  entries now respect the default source projection instead of returning the
  full document.

### Changed — chores

- release chores: landing version stamp rc.48 → rc.49
  ([#649](https://github.com/xerj-org/xerj/pull/649)), plus the routine
  release-downloads metrics snapshot.

## [1.0.0-rc.49] - 2026-08-23

A single-fix API release: `fields`/`docvalue_fields` now resolve named companion
fields through the default `_source`, plus the rc.48 landing stamp landing late.

### Fixed

- **`fields`/`docvalue_fields` resolve a named companion under the default
  `_source`** ([#310](https://github.com/xerj-org/xerj/issues/310),
  [#647](https://github.com/xerj-org/xerj/pull/647)). Previously a named
  companion field was missed when the default `_source` was in play; the fix
  pierces `_source` and re-narrows it at the emission sites named in the
  commits (the top-level `_source` and the scroll snapshot), and an
  unresolvable `docvalue_field` is now omitted rather than emitted as `[]`.

### Changed — chores

- Release chores: the v1.0.0-rc.47 → rc.48 landing stamp
  ([#645](https://github.com/xerj-org/xerj/pull/645)) landed inside this window.

## [1.0.0-rc.48] - 2026-08-23

An Elasticsearch-compatibility correctness release: two query/API semantics
fixes (`_source` glob filtering, one-clause `bool` scoring) plus regression
guards for collapse and rescore behavior.

### Fixed

- **Faithful ES `_source` glob automaton for nested paths**
  ([#602](https://github.com/xerj-org/xerj/issues/602),
  [#641](https://github.com/xerj-org/xerj/pull/641)). `_source` glob filtering
  now follows ES `makeAnyString` semantics — globs cross dots, non-accepting
  scalars are dropped, and dot-less globs route through the same automaton.

- **A one-clause `bool` is a no-op**
  ([#399](https://github.com/xerj-org/xerj/issues/399),
  [#642](https://github.com/xerj-org/xerj/pull/642)). A single-clause `bool`
  no longer changes `_score` and rank order.

### Added — tests

- **Collapse `inner_hits` page1==page2 byte-identity guard**
  ([#623](https://github.com/xerj-org/xerj/issues/623),
  [#640](https://github.com/xerj-org/xerj/pull/640)).
- **Real rescore ranking test + keyword-rescore guard**
  ([#627](https://github.com/xerj-org/xerj/issues/627),
  [#639](https://github.com/xerj-org/xerj/pull/639)).

Release chores: landing version stamp
([#638](https://github.com/xerj-org/xerj/pull/638)).

## [1.0.0-rc.47] - 2026-08-22

A one-fix release: a scroll API defect plus the routine landing stamp.

### Fixed

- **Scroll continuation honours `_source: false` from the opening request** ([#624](https://github.com/xerj-org/xerj/issues/624), landed via [#636](https://github.com/xerj-org/xerj/pull/636)).
  A scroll continuation was ignoring the `_source: false` setting made on the
  opening request, returning source documents anyway.

Release chores: landing version stamps for v1.0.0-rc.46 ([#635](https://github.com/xerj-org/xerj/pull/635)).

## [1.0.0-rc.46] - 2026-08-22

A small release: one targeted engine performance fix plus release chores.

### Performance

- **Residual gate skips Hit materialisation under `count_only`** ([#577](https://github.com/xerj-org/xerj/issues/577),
  [#633](https://github.com/xerj-org/xerj/pull/633)). When a count-only query
  takes the residual-gate path, hits are no longer materialised — the count is
  produced without building the `Hit` structures. Landed as part 1 of #577.

### Changed — chores

- **Release chores**: landing the v1.0.0-rc.45 version stamp
  ([#632](https://github.com/xerj-org/xerj/pull/632)).

## [1.0.0-rc.45] - 2026-08-22

A small scroll-versioning release: one API fix on the `_search_scroll` first page, one refactor unifying the scroll hit-render paths, and a version stamp.

### Fixed

- **`_search_scroll` first page carries `_seq_no`/`_version` when requested** ([#630](https://github.com/xerj-org/xerj/issues/630), landed via [#631](https://github.com/xerj-org/xerj/pull/631)).

### Changed

- **Scroll hit-render paths routed through `resolve_hit_versioning`** ([#614](https://github.com/xerj-org/xerj/issues/614), landed via [#629](https://github.com/xerj-org/xerj/pull/629)) — a refactor unifying the versioning logic ahead of the fix.

Release chores: landing version stamp for rc.44 ([#628](https://github.com/xerj-org/xerj/pull/628)).

## [1.0.0-rc.44] - 2026-08-22

A one-fix correctness release plus the rc.43 stamp: rescore-phase keyword
queries now respect the index mapping.

### Fixed

- **Keyword `match`/`multi_match` inside a rescore query is mapping-aware**
  ([#574](https://github.com/xerj-org/xerj/issues/574), merged via
  [#626](https://github.com/xerj-org/xerj/pull/626)). Previously the rescore
  phase analyzed keyword fields without regard to the mapping, so a keyword
  `match`/`multi_match` there behaved differently from the same query in the
  main phase.

Release chores: landing the v1.0.0-rc.43 version stamp
([#625](https://github.com/xerj-org/xerj/pull/625)).

## [1.0.0-rc.43] - 2026-08-22

A small correctness release: one API fix for collapse rendering on scrolled
pages, plus the rc.42 landing stamp.

### Fixed

- **Collapse `inner_hits` render on scroll continuation pages**
  ([#621](https://github.com/xerj-org/xerj/issues/621),
  [#622](https://github.com/xerj-org/xerj/pull/622)). The API failed to render
  collapse `inner_hits` on pages after the first of a scroll.

Release chores: landing version stamp for v1.0.0-rc.42
([#620](https://github.com/xerj-org/xerj/pull/620)).

## [1.0.0-rc.42] - 2026-08-22

A small correctness release: one `_source` filtering fix, plus release chores.

### Fixed

- **`_source` `prefix.*` include now requires the dot boundary** ([#602](https://github.com/xerj-org/xerj/issues/602), part of).
  A `prefix.*` include pattern was matching fields whose names merely started
  with `prefix` (e.g. `prefixfoo`); it now matches only `prefix.something`.
  Landed via [#619](https://github.com/xerj-org/xerj/pull/619).

Release chores: landing the v1.0.0-rc.41 version stamp ([#618](https://github.com/xerj-org/xerj/pull/618)).

## [1.0.0-rc.41] - 2026-08-22

A one-fix correctness release plus release chores.

### Fixed

- **Hybrid total reports `Eq` at the exact `per_query_topk` boundary** ([#594](https://github.com/xerj-org/xerj/issues/594),
  [#617](https://github.com/xerj-org/xerj/pull/617)). The hybrid query's total-hit
  count reported `Eq` when results landed exactly on the per-query top-k boundary.

Release chores: landing the v1.0.0-rc.40 version stamp ([#616](https://github.com/xerj-org/xerj/pull/616)).

## [1.0.0-rc.40] - 2026-08-22

A small correctness release: one scoring-semantics fix for multi_match queries,
plus release chores.

### Fixed — query semantics

- **`multi_match` `best_fields` over keyword+text fields now scores by `dis_max`** ([#572](https://github.com/xerj-org/xerj/issues/572),
  [PR #615](https://github.com/xerj-org/xerj/pull/615)). A keyword+text
  `best_fields` multi_match was being scored as a sum across fields instead of
  taking the best-matching field, over-weighting documents that matched both.

Release chores: landing the v1.0.0-rc.39 version stamp
([PR #613](https://github.com/xerj-org/xerj/pull/613)).

## [1.0.0-rc.39] - 2026-08-22

A small correctness release: one API fix for hit versioning metadata, plus
release chores.

### Fixed

- **Unresolved hits no longer fabricate `_seq_no`/`_version`** ([#603](https://github.com/xerj-org/xerj/issues/603),
  [#612](https://github.com/xerj-org/xerj/pull/612)). When a hit could not be
  resolved, the API had been inventing sequence-number and version values for
  it; it now omits those fields instead.

Release chores: landing version stamp for v1.0.0-rc.38
([#611](https://github.com/xerj-org/xerj/pull/611)).

## [1.0.0-rc.38] - 2026-08-22

A small release: one engine test-stability fix plus the rc.37 landing stamp.

### Fixed

- **Painless recursion tests no longer SIGABRT on a debug build** ([#353](https://github.com/xerj-org/xerj/issues/353),
  [#610](https://github.com/xerj-org/xerj/pull/610)).

Release chores: landing version stamp for v1.0.0-rc.37 ([#609](https://github.com/xerj-org/xerj/pull/609)).

## [1.0.0-rc.37] - 2026-08-22

A small correctness release: aggregation memory is now bounded by the
configured query-memory limit, plus the usual release chores.

### Fixed

- **Aggregation owned-source materialisation is bound to
  `max_query_memory_mb`** ([#464](https://github.com/xerj-org/xerj/issues/464),
  [#608](https://github.com/xerj-org/xerj/pull/608)). Both aggregation paths
  now bound their corpus memory under the setting instead of materialising
  unbounded owned sources.

### Changed — chores

- release chores: landing version stamp-bump v1.0.0-rc.35 → rc.36
  ([#607](https://github.com/xerj-org/xerj/pull/607)).

## [1.0.0-rc.36] - 2026-08-22

A one-fix autoindex release plus release chores: Java constant symbols now land
in the autoindex, and the landing page version stamp moved rc.34 → rc.35.

### Fixed — autoindex

- **Java static constants indexed as their own symbols** ([#500](https://github.com/xerj-org/xerj/issues/500)).
  Static and interface constants were not previously emitted as standalone
  symbols by the autoindex ([#605](https://github.com/xerj-org/xerj/pull/605)).

### Changed — chores

- release chores: landing version stamp-bump rc.34 → rc.35
  ([#606](https://github.com/xerj-org/xerj/pull/606)).

## [1.0.0-rc.35] - 2026-08-22

An Elasticsearch-protocol correctness release: three targeted fixes — keyword
query rewriting inside vector/score-filter contexts, collapse inner_hits
metadata honesty, and `_source` dotted-filter handling of empty objects.

### Fixed

- **Keyword `match`/`multi_match` are mapping-aware inside
  Knn/SemanticSearch/function_score filters** ([#574](https://github.com/xerj-org/xerj/issues/574),
  [#604](https://github.com/xerj-org/xerj/pull/604)).
- **Collapse `inner_hits` omit absent snapshot `_seq_no`/`_version`**
  instead of fabricating values ([#566](https://github.com/xerj-org/xerj/issues/566),
  [#601](https://github.com/xerj-org/xerj/pull/601)).
- **An already-empty object is preserved under a dotted `_source` filter**,
  with the relaxation gated to an accepting state and the accept test made
  ES-correct for `.*` globs ([#310](https://github.com/xerj-org/xerj/issues/310),
  [#599](https://github.com/xerj-org/xerj/pull/599)).

### Changed — chores

- Release chores: landing version stamp-bump rc.33 → rc.34
  ([#600](https://github.com/xerj-org/xerj/pull/600)).

## [1.0.0-rc.34] - 2026-08-22

A single-fix engine release: the schema-evolution throttle was keyed too
coarsely, plus the routine version stamp for the prior cut.

### Fixed

- **Schema throttle re-evolves on value-shape changes** ([#382](https://github.com/xerj-org/xerj/issues/382),
  [#598](https://github.com/xerj-org/xerj/pull/598)). The evolution throttle
  fingerprinted only field names, so a refused key never re-evolved when the
  leaf value type changed; the leaf value-type is now folded into the hash,
  and the fingerprint recurses into array elements so array-shape transitions
  re-evolve too. Landed with an end-to-end guard test and a correction to the
  accompanying hash-reach comments.

### Changed — chores

- Release chores: landing version stamp v1.0.0-rc.32 → rc.33
  ([#597](https://github.com/xerj-org/xerj/pull/597)).

## [1.0.0-rc.33] - 2026-08-22

A small correctness release: one API fix for `_eql/search` event ordering, plus release chores.

### Fixed

- **`_eql/search` orders events by `@timestamp` instant across alias members before truncating** ([#567](https://github.com/xerj-org/xerj/issues/567), [#596](https://github.com/xerj-org/xerj/pull/596)).
  Events from multiple alias members were previously not globally ordered by
  `@timestamp` before the result set was truncated, so events could be cut
  out of order.

### Changed — chores

- release chores: landing version stamp bump v1.0.0-rc.31 → rc.32 ([#595](https://github.com/xerj-org/xerj/pull/595)).

## [1.0.0-rc.32] - 2026-08-22

A small correctness release: one wire-protocol semantics fix for hybrid/fusion
totals, one test-flake fix, and release chores.

### Fixed

- **Hybrid/fusion totals report `relation:gte` when capped** ([#569](https://github.com/xerj-org/xerj/issues/569)).
  A capped total was previously reported as a false `eq`; it is now surfaced as
  `relation:gte` ([#593](https://github.com/xerj-org/xerj/pull/593)).
- **Stale HNSW rebuild test no longer flakes** ([#536](https://github.com/xerj-org/xerj/issues/536)).
  The test now deterministically awaits the rebuild instead of racing it
  ([#592](https://github.com/xerj-org/xerj/pull/592)).

### Changed — chores

- Release chores: landing version stamp-bump rc.30 → rc.31
  ([#591](https://github.com/xerj-org/xerj/pull/591)) and metrics-only
  release-download collection (no engine change).

## [1.0.0-rc.31] - 2026-08-22

A one-fix autoindex correctness release, plus a landing-page version stamp.

### Fixed

- **Autoindex no longer treats an excluded-but-present file as a deletion** ([#439](https://github.com/xerj-org/xerj/issues/439),
  [#590](https://github.com/xerj-org/xerj/pull/590)). Files excluded from
  indexing while still on disk — including those with non-UTF-8 names — were
  being misread as deletions; the two cases are now distinguished.

### Changed — chores

- Release chores: landing stamp-bump v1.0.0-rc.29 → rc.30
  ([#588](https://github.com/xerj-org/xerj/pull/588)).

## [1.0.0-rc.30] - 2026-08-22

An autoindex-correctness release: three fixes to how the autoindexer sniffs
files and guards corpus re-runs, plus a landing version stamp.

### Fixed — autoindex

- **Markdown-with-frontmatter is sniffed as its body, not YAML** ([#551](https://github.com/xerj-org/xerj/issues/551)).
  The sniffer treated the frontmatter as the document, so the indexed content
  was the metadata block rather than the prose.

- **A graph-path re-run of a `--no-graph` corpus is refused before it mutates**
  ([#490](https://github.com/xerj-org/xerj/issues/490)). The cross-path
  mutation was previously caught only after the damage; the check now precedes
  it, and the regression test was made load-bearing.

- **No more false "identity changed" for non-resumable backends**
  ([#487](https://github.com/xerj-org/xerj/issues/487)). A backend that
  simply cannot resume was misreported as a corpus identity change.

Release chores: landing stamp-bump v1.0.0-rc.28 → rc.29
([#582](https://github.com/xerj-org/xerj/pull/582)).

## [1.0.0-rc.29] - 2026-08-21

A small autoindex-correctness release: two fixes to the code autoindex/re-index
path, plus a version stamp. The code-indexing pipeline had been collapsing
declarations together and aborting on a schema edge case.

### Fixed

- **Each code declaration is indexed as its own retrievable document**
  ([#500](https://github.com/xerj-org/xerj/issues/500),
  [#579](https://github.com/xerj-org/xerj/pull/579)). Previously declarations
  were not individually retrievable; per-declaration records are also deduped
  by locator.

- **Re-index no longer aborts on a field absent-but-unscored in the frozen
  schema** ([#580](https://github.com/xerj-org/xerj/issues/580),
  [#581](https://github.com/xerj-org/xerj/pull/581)). The re-index run
  previously treated such fields as fatal.

Release chores: landing version stamp rc.27 → rc.28
([#578](https://github.com/xerj-org/xerj/pull/578)).

## [1.0.0-rc.28] - 2026-08-21

A one-fix correctness release: a bool-filter scoring defect, plus the rc.27
version stamp landing in the same window.

### Fixed

- **Non-projectable bool `filter`/`must_not` no longer diverts the scorer**
  ([#361](https://github.com/xerj-org/xerj/issues/361)).
  A bool query whose filter or must_not clause was non-projectable was
  diverting the scorer; it no longer does
  ([#576](https://github.com/xerj-org/xerj/pull/576)).

### Changed — chores

- Release chores: landing the v1.0.0-rc.26 → rc.27 version stamp
  ([#575](https://github.com/xerj-org/xerj/pull/575)).

## [1.0.0-rc.27] - 2026-08-21

An ES-compatibility correctness release: hybrid `knn` + `query` was not actually
hybrid, keyword `match`/`multi_match` ignored the mapping (in the memtable and
inside wrapper queries), and `cat_count` swallowed per-member errors.

### Fixed

- **`knn` beside `query` now folds to a true hybrid only when hybrid-safe**
  ([#458](https://github.com/xerj-org/xerj/pull/458)). Previously the lexical
  answer was returned and labelled hybrid; the rework completes the
  `hybrid_safe` guard for every field `run_hybrid` drops, with tests covering
  the union result and aggs-200.
- **Keyword `match`/`multi_match` is mapping-aware** in the memtable
  ([#354](https://github.com/xerj-org/xerj/issues/354)) and inside wrapper
  queries (the #354 residual), including boosted `multi_match` after stripping
  `^boost` ([#571](https://github.com/xerj-org/xerj/pull/571),
  [#573](https://github.com/xerj-org/xerj/pull/573)).
- **`cat_count` propagates per-member errors** instead of undercounting
  ([#563](https://github.com/xerj-org/xerj/issues/563),
  [#568](https://github.com/xerj-org/xerj/pull/568)).

Release chores: site release stamps bumped to v1.0.0-rc.26
([#570](https://github.com/xerj-org/xerj/pull/570)).

## [1.0.0-rc.26] - 2026-08-21

An alias-semantics and honesty release: filtered-alias reads fixed across the remaining ES-compat endpoints, a collapse inner_hits metadata fix, a semantic-clause dispatch fix, and a benchmark-harness correction alongside site documentation cleanup.

### Fixed

- **Alias filters applied on `_msearch`** ([#451](https://github.com/xerj-org/xerj/issues/451) class C, [#559](https://github.com/xerj-org/xerj/pull/559)). The same class-C pass also covers `_msearch/template`, `_cat/count`, and a scroll test guarding that scroll honours a filtered alias.
- **`_eql/search` fans out to every alias member** ([#451](https://github.com/xerj-org/xerj/issues/451), the last class-B read site, [#561](https://github.com/xerj-org/xerj/pull/561)); the alias filter is applied on the fan-out, closing a filtered-alias leak.
- **Collapse `inner_hits` carry snapshot `seq_no`/`version`** ([#506](https://github.com/xerj-org/xerj/issues/506), [#562](https://github.com/xerj-org/xerj/pull/562)).
- **Single-clause `bool` wrapping a semantic clause now dispatches** ([#395](https://github.com/xerj-org/xerj/issues/395) case B, [#557](https://github.com/xerj-org/xerj/pull/557)).

### Changed

- **Harbor 3-arm benchmark honesty** ([#516](https://github.com/xerj-org/xerj/issues/516), [#564](https://github.com/xerj-org/xerj/pull/564)): partial arm coverage is now flagged and the tuned prompt's pre-asserted claim neutralized.
- **Site docs: built-in neural embedder documented**, stale tally corrected and dead links fixed; `max_process_memory_mb` dropped from the config reference table ([#492](https://github.com/xerj-org/xerj/issues/492), [#565](https://github.com/xerj-org/xerj/pull/565)).

Release chores: version stamps bumped to v1.0.0-rc.25 on the site ([#558](https://github.com/xerj-org/xerj/pull/558)).

## [1.0.0-rc.25] - 2026-08-21

A small ES-wire-compatibility release: one shared alias/index-selector resolver
migrating read endpoints off first-index-only behavior, plus a regression test
locking filtered-alias by-query delete safety.

### Fixed

- **Shared index-selector resolver for alias-aware read endpoints**
  ([#451](https://github.com/xerj-org/xerj/issues/451),
  [#555](https://github.com/xerj-org/xerj/pull/555)). Read endpoints resolved
  through multi-index aliases by taking only the first member; `_mget`,
  `_explain`, `_cat/segments`, and `_segments` now migrate onto a shared
  resolver (class B of #451). A direct commit reproduces the original `_mget`
  failure before the fix.

### Added

- **Regression coverage for filtered-alias by-query over-delete**
  ([#553](https://github.com/xerj-org/xerj/issues/553),
  [#556](https://github.com/xerj-org/xerj/pull/556)). Tests lock the
  filtered-alias by-query slice safety and `_update_by_query` reporting
  aggregate `deleted:0`.

Release chores: site release stamps bumped to v1.0.0-rc.24
([#554](https://github.com/xerj-org/xerj/pull/554)).

## [1.0.0-rc.24] - 2026-08-21

A small ES-compatibility fix release: `_delete_by_query`/`_update_by_query`
through a multi-index alias, plus regression tests locking two kNN invariants.

### Fixed — ES compatibility

- **By-query APIs fan out to every alias member** ([#450](https://github.com/xerj-org/xerj/issues/450)).
  `_delete_by_query`/`_update_by_query` issued through a multi-index alias
  touched only the first member; they now apply to all members
  ([#552](https://github.com/xerj-org/xerj/pull/552)).

### Changed — tests

- **Locked multi-knn and nested-knn 200 invariants** ([#547](https://github.com/xerj-org/xerj/issues/547)):
  declared-but-empty knn sections must return 200
  ([#549](https://github.com/xerj-org/xerj/pull/549)).

Release chores: site release stamps bumped to rc.23 ([#548](https://github.com/xerj-org/xerj/pull/548)).

## [1.0.0-rc.23] - 2026-08-20

A small correctness release: one wire-protocol fix for nested kNN clauses, a shipped
API regression test, and release chores.

### Fixed

- **Unanswerable field in a nested `knn` clause returns 400** ([#542](https://github.com/xerj-org/xerj/issues/542)).
  The nested arm of the kNN validator now rejects such clauses with a client error
  instead of failing to answer ([#546](https://github.com/xerj-org/xerj/pull/546)).

### Added

- **Regression test for semantic query on a non-`semantic_text` field**
  ([#530](https://github.com/xerj-org/xerj/issues/530)). The negative case is now
  covered, and the 400 response names the offending field
  ([#545](https://github.com/xerj-org/xerj/pull/545)).

Release chores: bumping site release stamps to v1.0.0-rc.22 ([#544](https://github.com/xerj-org/xerj/pull/544)).

## [1.0.0-rc.22] - 2026-08-20

A small kNN-correctness release: two fixes making kNN/semantic queries on
unanswerable fields return a 400 instead of a silent empty 200, plus release
stamps.

### Fixed

- **400 a knn/semantic clause on an unanswerable field** ([#498](https://github.com/xerj-org/xerj/issues/498),
  [#541](https://github.com/xerj-org/xerj/pull/541)). A single top-level knn
  targeting a field that cannot answer it no longer returns a silent empty 200.
- **400 an unanswerable field in a multi-knn array clause** ([#542](https://github.com/xerj-org/xerj/issues/542),
  [#543](https://github.com/xerj-org/xerj/pull/543)). The same semantics
  extended to the multi-knn array form.

Release chores: bumping site release stamps to v1.0.0-rc.21
([#540](https://github.com/xerj-org/xerj/pull/540)).

## [1.0.0-rc.21] - 2026-08-20

A small correctness-and-hygiene release: two columnar-storage fixes, a CI
unblock, an end-to-end test, and site housekeeping.

### Fixed

- **ZBS3 routed through the columnar retained-size estimator**
  ([#538](https://github.com/xerj-org/xerj/issues/538),
  [#539](https://github.com/xerj-org/xerj/pull/539)). The ZBS3 storage path
  now goes through the columnar retained-size estimator instead of bypassing
  it.
- **Explicit `_source` nulls survive the columnar codec**
  ([#415](https://github.com/xerj-org/xerj/issues/415),
  [#535](https://github.com/xerj-org/xerj/pull/535)). Explicit nulls in
  `_source` are preserved through the columnar codec rather than dropped.
- **Clippy 1.98 satisfied across the workspace**
  ([#537](https://github.com/xerj-org/xerj/pull/537)). Unblocks the Format +
  Clippy CI jobs.
- **sitemap.xml covers the zero-token and semantic-analytics pages**
  ([#534](https://github.com/xerj-org/xerj/pull/534)).

### Added

- **End-to-end embedder-swap refusal test**
  ([#522](https://github.com/xerj-org/xerj/issues/522),
  [#532](https://github.com/xerj-org/xerj/pull/532)).

### Changed — chores

- Release chores: landing release stamps bumped, metrics release-download
  counts captured (1148 assets, 624 binaries).

## [1.0.0-rc.20] - 2026-08-19

A small release: one engine fix (alias metadata persistence) and a batch of
site/docs corrections ahead of it.

### Fixed

- **Alias filter metadata survives a restart** ([#524](https://github.com/xerj-org/xerj/issues/524),
  [#525](https://github.com/xerj-org/xerj/pull/525)). Filtered-alias metadata
  is now persisted instead of being lost across restarts.
- **`llms.txt` "Current release" pointed at rc.19** ([#515](https://github.com/xerj-org/xerj/issues/515),
  [#520](https://github.com/xerj-org/xerj/issues/520),
  [#527](https://github.com/xerj-org/xerj/pull/527)). Corrected, with a guard
  added so the stamp cannot drift again.

### Documentation

- **Zero-Token AI Search use-case page** ([#523](https://github.com/xerj-org/xerj/pull/523)).
  Three real scenarios: code, PDF, Markdown.
- **Snapshot/restore scoping corrected** ([#400](https://github.com/xerj-org/xerj/issues/400),
  [#528](https://github.com/xerj-org/xerj/pull/528)). The `wal_tap` docs now
  state snapshot/restore seeds a XERJ target only, not a cross-engine one.
- **Honest "first hit" example**: the site query was replaced with one where
  sniff.rs genuinely ranks first.

### Changed — chores

- Release chores: footer release stamps bumped to v1.0.0-rc.19
  ([#526](https://github.com/xerj-org/xerj/pull/526)).

## [1.0.0-rc.19] - 2026-08-19

A broad correctness-and-honesty release: ES-compat fixes for aliases, scroll and
sort; a stepped default memory cap; an embedding-mode safety guard; plus a wave
of README/landing truth-alignment and two field-report harnesses.

### Fixed — engine and ES-compat

- **Multi-index aliases answer over all members** ([#449](https://github.com/xerj-org/xerj/pull/449)).
  A multi-index alias had been answering as an alias over one member on every
  read path; alias filters are now read from the names as written and applied
  in `_count` too.
- **Scroll carries real `_seq_no`/`_version`** ([#440](https://github.com/xerj-org/xerj/issues/440))
  on the same read as `_source`, and the summed scroll snapshot cap is now
  enforced on the alias route as well ([#405](https://github.com/xerj-org/xerj/issues/405)).
- **Sort on an unresolvable field is rejected, honouring `unmapped_type`**
  ([#437](https://github.com/xerj-org/xerj/issues/437)) instead of
  null-stranding `search_after`.
- **Embedding-mode guard**: the engine refuses to open an index whose vectors
  were produced under a different `embedding.mode` ([#446](https://github.com/xerj-org/xerj/pull/446));
  the resolved embedder is recorded in a separate sidecar, not the mode string.
- **Stepped default memory cap** ([#477](https://github.com/xerj-org/xerj/pull/477)):
  8/16/32 GiB by RAM instead of a flat 8 GiB, with both memory env knobs
  accepting `auto|off|unlimited|<MiB>` ([#502](https://github.com/xerj-org/xerj/issues/502)).
- **`%PDF-` magic signature qualified** in autoindex ([#403](https://github.com/xerj-org/xerj/issues/403));
  first-launch console link prints the bound ES-compat port, not the requested
  one ([#495](https://github.com/xerj-org/xerj/pull/495)); `feedback --open-pr`
  commits only the field report, not the whole staged index ([#484](https://github.com/xerj-org/xerj/issues/484));
  de-flaked `bool_deadline_never_broadens_a_must` ([#510](https://github.com/xerj-org/xerj/issues/510)).

### Added

- **Reproducible 3-arm Harbor benchmark harness** (Claude Code vs XERJ vs
  tuned, [#513](https://github.com/xerj-org/xerj/pull/513)) with measured fixes
  from the first parallel campaign night ([#521](https://github.com/xerj-org/xerj/pull/521)),
  and a field report running Frontier-Bench v0.1 as a 3-arm XERJ A/B
  ([#512](https://github.com/xerj-org/xerj/pull/512)) plus one on building a
  token benchmark against Lucene ([#494](https://github.com/xerj-org/xerj/pull/494)).

### Changed — docs and CI

- README reworked in three passes (badges-first plain prose, one value-first
  opening paragraph, then repositioned around reference coding and token
  savings); landing pages verify release downloads ([#454](https://github.com/xerj-org/xerj/issues/454));
  the public-sector page drops at-rest-crypto claims the build does not ship
  ([#491](https://github.com/xerj-org/xerj/issues/491)).
- CI: the landing-constants-guard is wired in and the site re-stamped
  ([#489](https://github.com/xerj-org/xerj/issues/489)); main's CI runs are no
  longer cancelled, so the badge stops showing cancelled-as-failing
  ([#503](https://github.com/xerj-org/xerj/pull/503)).

## [1.0.0-rc.18] - 2026-08-18

### Security

- **Patched `h2` for RUSTSEC-2026-0258, "unbounded empty DATA frames"** (0.4.13 ->
  0.4.16). `h2` is the HTTP/2 implementation beneath `hyper`, so it sits under
  every listener this server runs — ES-compat and native REST through axum, gRPC
  through tonic. An unbounded stream of empty DATA frames was a remote
  resource-exhaustion vector against any reachable node; the patched version
  answers with `GOAWAY ENHANCE_YOUR_CALM` instead.

  The fix adds a per-connection budget, and it is a **token bucket, not a
  quota** — though, as below, that is not enough to keep it off real traffic.
  `h2-0.4.16/src/proto/streams/counts.rs:94-101`: a DATA frame *under* 256 bytes
  consumes `256 - payload_len`; a frame of 256 bytes or more **replenishes**
  `payload_len - 256`. The refill stops at a 25,600-byte ceiling, which lives
  elsewhere — `Budget::new(DEFAULT_DATA_FRAME_BUDGET)` at `counts.rs:89` and the
  `.min(self.max)` at `counts.rs:26`, over `DEFAULT_DATA_FRAME_BUDGET = 256 * 100`
  at `proto/mod.rs:39`. Any ordinary payload frame
  therefore refills what small frames drain — but only a frame that reaches 256
  bytes does. A body chunked below that is made entirely of frames that drain,
  which is what the paragraphs below are about.

  Two further details decide whether you can ever meet it. A small frame *with*
  a payload is refunded when the application reads it (`streams.rs:1515`) — but
  only once the read happens, which is later than you would think and is the
  subject of the next paragraph. A frame with an **empty payload**
  is not: h2 discards it before the application sees it (`recv.rs:757` returns
  early for an empty frame without END_STREAM), so its 256 is never refunded —
  only offset by a large frame elsewhere on the connection. "Empty" here means
  zero *payload*, not zero bytes on the wire: a padding-only frame counts, and
  so does a small frame on a stream that is reset before the application reads
  it. An empty frame carrying END_STREAM is delivered, and is refunded. Exhaustion is a connection-level
  `GOAWAY ENHANCE_YOUR_CALM`, and it takes the **101st** empty frame with
  nothing ≥256 bytes in between (`checked_sub` fails once `available` reaches 0).

  Reading the body is **not** by itself protection, and this is the part to take
  operationally. The charge lands on arrival and the refund only on the read, so
  once a flow-control window's worth of frames has arrived before the handler is
  scheduled, the connection is already over budget. Measured on this build, a
  handler consuming every frame in a tight loop is cut off at exactly the same
  frame as one that never reads: 101 at 1 byte, 134 at 64, 201 at 128, 356 at
  184. XERJ advertises `SETTINGS_INITIAL_WINDOW_SIZE = 1048576`, far above h2's
  64 KiB default, so more frames buffer here than elsewhere before a read lands.

  The safe line is frame size, not frame count, and it is sharp: **a frame of 256
  bytes or more never charges at all**. The trip frame for a smaller size is
  `floor(25600 / (256 - size)) + 1` — the first frame whose charge underflows the
  remaining budget. Below roughly 200 bytes a legitimate, authenticated,
  fully-read request can take the `GOAWAY`: a valid 27 KB `_bulk` chunked into
  128-byte DATA frames did so repeatedly across runs, and a 307 KB body at 184
  bytes likewise. How often depends on load and on how the client frames and
  drains — measurements on two machines differed in both directions, so treat
  the hazard as real and the frequency as unpredictable rather than as a rate.
  Tracked as [#485](https://github.com/xerj-org/xerj/issues/485).
  If you control the client, chunk at 256 bytes or above. **On this release
  only** — see the Unreleased entry for #485 above, where the `h2` floor moves
  to 0.4.19 and the budget starts scaling with the connection window.

  XERJ has such a path: `auth_middleware` is an axum layer, so a request that
  fails authentication is answered `401` without the handler ever extracting its
  body (`xerj-api/src/auth.rs`). An unauthenticated client that streams many
  small frames at a node with auth enabled therefore accumulates charges nothing
  refunds, and the connection is eventually closed with `GOAWAY`. Authentication is not special
  here: any rejection that answers without reading the body does the same, so an
  *authenticated* client that mistypes a route and streams a small body into the
  404 sees the identical `GOAWAY`. Worth recognising for what it is rather than
  reading it as a client bug.

  Two `unsound` advisories were cleared in the same pass: `anyhow` 1.0.102 ->
  1.0.104 (RUSTSEC-2026-0190, `Error::downcast_mut()`) and `memmap2` 0.9.10 ->
  0.9.11 (RUSTSEC-2026-0186, unchecked pointer offset), plus the yanked
  `spin` 0.9.8 -> 0.9.9 (#483).

### Fixed

- **A node whose ES-compat port was taken printed a success banner and stayed
  up, and `xerj autoindex` then wrote the user's documents into whichever other
  node owned that port.** The three listeners were bound inside tasks spawned
  *after* the banner, where a bind error was logged and explicitly non-fatal, so
  losing one port was survivable. The process that does own the port answers
  `GET /_cluster/health` with `green` — the readiness probe the docs prescribe —
  so nothing downstream could tell the difference. Found by three independent
  agents in a field study; one of them wrote 905 files, including contracts,
  invoices and a bank export, into a stranger's data directory and got
  `ok=true exit=0` for it. Every listener is now bound before the banner, a
  refused bind ends startup with the port and the reason on stderr, and the
  banner is printed from the addresses the kernel actually returned (#465,
  #466).

- **A `keyword` field holding a JSON array was flattened into one token, and the
  answer changed at `_flush`.** Two independent causes, which is why the symptom
  looks inconsistent. In a segment the array was joined into a single string
  before the FTS layer saw it, and the keyword analyzer emits its whole input as
  one token — so every clause that projects to a whole-value `FtsQuery::Term`
  missed the document, the first element included; measured on
  `{"tags":["red","blue"]}`, after a flush both `red` and `blue` returned 0 hits
  and only the joined artefact `"red blue"` matched. The type name is
  load-bearing: `terms` reads like such a clause but does not compile to one —
  it routes to the doc-values path, which already bailed on array fields, and it
  returned the document correctly on both sides of the flush. Before the flush a
  different cause applied: the memtable's single-valued columns keep only element
  0, and the fused columnar walk did not bail out on such a field the way its
  sibling paths did. That walk serves `term` and `range`, and `bool` with `must`
  or `filter`, through `constant_score` and boosted wrappers — so a `bool`/
  `filter` query was affected too, not only a bare `term`. There `red` matched
  and `blue` did not. The
  first-element-survives shape people remember is the pre-flush one and does not
  describe a flushed index. Array elements are now indexed as N independent
  values separated by a position gap, so a phrase query cannot span two elements,
  and the columnar path refuses an array-valued field rather than silently
  reading only its first element (#332, #470).

- **The audit log recorded no writes and could not name the actor, and
  `/_audit` was readable by any credential.** Create, index, update, bulk and
  delete appended nothing; the one audited data-path op recorded the literal
  `"anonymous"` for an authenticated admin call (#329, #471).

- **`bool` scoring: a scoring-irrelevant clause collapsed `_score` and reordered
  the page — partially fixed.** A `filter`, a second `must`, or a `must_not`
  changed relevance scores and hit order, with no error and no warning. Three
  defects of the same shape, all of them a clause or a pass affecting scoring
  when it must not: `bool.filter` was projected onto the FTS `must` slot so its
  BM25 landed in `_score`; the IDF rescore counted `filter`/`must_not` toward
  its trigger; and that rescore derived IDF from `final_hits.len()`, which made
  `_score` a function of `size`.

  **Read the scope before you re-baseline. This fix is partial and #361 remains
  open.** What is fixed: a `filter` or `must_not` carrying a `term`-shaped
  child, on a page served entirely by the segment FTS path. What is *not*:
  `filter: [{match_all: {}}]` and `filter: [{exists: ...}]` still return a flat
  score over unrelated documents, and any page that also carries memtable or
  stored-scan hits keeps rc.17 behaviour — that fallback is IDF-less
  (`1 + ln(1 + tf)`) and was deliberately untouched. On those shapes `_score`
  still varies with `size`; measured on rc.18, one query returned four different
  top-1 documents at `size` 2 / 5 / 10 / 50.

  **Where it does apply, this changes `_score` values and hit ordering versus
  rc.17.** If you have pinned expected scores, recorded relevance baselines, or
  tests asserting an exact order, re-baseline them on rc.18. The new behaviour
  is the correct one — Lucene draws the same line at
  `BooleanClause.isScoring()` — but a silent ranking change is exactly the kind
  of thing that should not arrive unannounced (#361, #387).

- **`xc.py` could not tell a corpus that was never loaded from a query that
  genuinely matched nothing** (#476).

- **An alias 404'd on `_refresh`, `_forcemerge`, `_cache/clear` and
  `_terms_enum`.** `resolve_indices_for_op` had no alias branch, so a valid
  alias was simply "not an index". It now expands to every member rather than
  the first — a `_refresh` reaching one of three members makes the next search
  non-deterministic with nothing in the response saying so (#459). The remaining
  read-path half — an alias answered as an alias over one member — is still open
  (#449).

- **The air-gapped deployment recipe failed open: a bad digest extracted and
  installed anyway.** The verify step did not stop extraction, and because
  `set -eu` sat at the top of the block rather than inside it, a failure killed
  the operator's shell — so the natural recovery, which the page itself named,
  was to re-paste without it, and that made the digest check non-fatal. Every
  block that decides something now runs inside its own subshell, the digest is
  computed locally and asserted against the `.sha256` with `grep -qxF`, and the
  block that verifies is the block that installs (#441).

- **The install page's checksum step never hashed the archive.** It ran
  `sha256sum -c`, which reports success for whatever filenames the `.sha256`
  happens to list, skips `#` comment lines silently, and never checks that any
  line names the archive being extracted. A `.sha256` carrying a comment naming
  the archive plus a valid digest for an unrelated file verified clean at exit 0
  while the archive was never hashed. Both the install page and `llms.txt` now
  compute the digest, assert it is 64 hex characters, demand that exact line,
  and chain extraction and install to the result (#444, #452).

- **A text file whose first two characters were `BM`, or whose first four were
  `GIF8`, was junked as an image and never indexed.** They were the only two
  entirely-printable signatures *in the magic table* accepted without a
  qualifier — the scoping matters, and the rest of this entry says why — so `sniff`
  returned `Family::Binary` and `scan_file` skipped the file citing "binary
  content (gif)" / "(bmp)" — a reason naming a format the file did not have. The
  reported case is a CSV whose first column header is `BMW`; a health export
  headed `BMI` fails the same way, as does prose beginning "BM" or "GIF8".
  rc.17 shipped this knowingly and said so twice, pinning the wrong answers in
  `gif8_and_bm_are_still_taken_on_faith` so the deferral stayed visible. That
  test is replaced by `text_that_opens_with_gif8_or_bm_is_text`, and
  `every_printable_signature_carries_a_qualifier` now walks every row of the
  signature table so the defect cannot return *through that table*. It is not
  the whole class: `%PDF-` is matched earlier, is equally printable, and prose
  opening with it is still handed to the PDF extractor — deliberately pinned and
  still open as #403.

  **A corpus indexed before this release will not necessarily pick the file up
  on a re-run.** Two things decide it and they are easy to confuse: the indexing
  path decides whether the file is re-sniffed at all, and the state dir decides
  whether anything can then accept it. The cases differ enough that the only
  guidance worth giving is: check the exit code, and read the error if there is
  one.

  On the default (graph) path the frozen plan is reused instead of re-sniffed, so
  a file recorded in `plan.junk_files` is never reconsidered: the re-run leaves
  the index unchanged and exits 3 (`completed-with-junk`), the same code the
  original run gave — there is no signal that anything is now recoverable.

  With `--no-graph` the file is re-sniffed and then has to be re-admitted, which
  is decided by four gates in `classify_new` (`reconcile_plan.rs:279-323`) — the
  family/group filter, `ensure_compatible`, a field-overlap threshold, and a
  refusal when two frozen datasets match equally well — which between them select
  one of three outcomes. All three are
  reachable — the file is recovered in place, or the run aborts naming it, or it
  stays junk — and which one you get depends on what the other files in the
  corpus froze into the plan. This entry deliberately does not predict it: six
  attempts to state the rule concisely were each refuted by measurement, and the
  honest instruction is to run it and read the exit code. A `csv` file rejoining
  a compatible `csv` dataset is recovered incrementally in seconds; prose never
  is, because prose datasets record `family = docs` while the file sniffs
  `txt-prose`, so the first gate can never match.

  A new `--state-dir` with a new `--prefix` recovers the file in every
  configuration measured. `--fresh` recovers it in some and is refused in
  others — the refusal tracks what the state dir contains, not which flag you
  pass. Both re-extract the whole corpus and re-embed it on a semantic index.
  The new-prefix route also leaves the old prefix's indices live and serving
  until you delete them by hand, so budget for two copies of the corpus in the
  interim.
  The full matrix, including the cross-path cases where a corpus indexed one way
  is re-run the other, is #490.

  A residual class stays `Family::Binary` regardless: the GIF arm junks a file
  whose prefix fills the sniff buffer **or** whose bytes cannot be written as a
  canvas dimension, and the second disjunct has no size floor — the release's
  own tests pin a 3,000-byte prose file junked as an image under a 1 MiB budget.
  Describing the cost as "large files only" would name one half of it
  (#379, #380, #427).

- **A GIF whose comment chain outruns the sniff budget is decided by the chain,
  not by a canvas heuristic** (#427, #442).

### Added

- **`xerj feedback`** — one command drafts the field report the project asks every
  agent to file, auto-filling version, OS and what was indexed;
  `xerj feedback --open-pr` stages that report under
  `user-feedback/16-agent-field-reports/` and opens the PR — it stages the report
  but does not restrict the commit to it, so check `git status` first if you have
  other work staged. Field-report PRs are CLA-exempt, so they merge without a
  signature (#473).

- **A build gate against release notes that promise the next release.** A file
  shipping inside the release tag may no longer describe a capability as arriving
  later — that sentence cannot be checked at review time and is wrong by
  construction afterwards, whichever way it resolves. Both halves of that shipped
  during this cycle. The gate is a list of known phrasings rather than a rule
  about meaning, and says so; #474 tracks what would close the class (#474, #479).

- **`word_delimiter` / `word_delimiter_graph` token filters and a `code`
  analyzer**, so an identifier can be matched by its sub-words
  (`getHTTPResponse` -> `get` / `HTTP` / `Response`) while the whole identifier
  is preserved for exact matches. Capability only — no performance claim (#468).

- A reproducible verification harness for the neural embedder's missing
  batching, measuring lexical 1.3 s against neural 677.5 s on an identical
  101-file slice. This is the measurement, not the fix — **#366 stays open**
  (#467).

- Operational-recipe verification protocol in `docs/CONTRIBUTION_REVIEW.md`,
  including the two clauses that would have caught the round of #441 that went
  green while still broken: run each negative control with the following block
  appended, and with the failing block's exit status discarded (#447, thanks
  @buger).

- `docs/XERJ_VS_LUCENE.md` — a side-by-side comparison with Lucene 10.3.1,
  honest in both directions (#373, #448, thanks @buger).

### Changed

- **XERJ no longer sizes its memory budgets to the whole machine. New default:
  8 GiB.** Every budget derived from `effective_memory_limit_bytes()`, which is
  min(cgroup limit, total system RAM) — so with no cgroup, the normal laptop and
  macOS case, a bigger machine bought a hungrier XERJ. Measured on a 121 GiB
  host, the derivation base was 124,609 MB, giving a 31,152 MB memtable budget
  and a 24,921 MB hydration cache before anything else allocated. Users reported
  ~20 GiB resident for two indexed projects.

  `limits.max_process_memory_mb` now defaults to `8192` and caps the derivation
  base itself, so every dependent budget shrinks coherently. It only ever
  lowers: a 4 GiB laptop stays 4 GiB and a smaller cgroup limit still wins. Set
  `0` to restore the previous machine-proportional behaviour on a dedicated box
  (#461).

  **Read this before relying on it as a ceiling.** It caps the budgets derived
  from machine size. It is NOT an RSS limit, and an audit of this release
  measured a node configured with exactly these budgets reaching 7,924 MB
  resident while the capped caches held 873 MB — the governed budgets cover
  under half of peak RSS, and the rest is merge-path allocation, aggregation
  workspace and allocator retention that consult no budget at all. On a large
  corpus the cap can therefore surface as an HTTP 429 on ingest rather than as
  flat memory. If you index a large project and see 429
  `circuit_breaking_exception`, raise `limits.max_process_memory_mb`. Reducing
  the memory that is outside these budgets is follow-up work, not something this
  release completes.

- **The README's lead was rewritten three times in this release; the third is
  what ships.** #457 moved the lead from reference coding to `autoindex`, on the
  argument that reference coding is one use case standing in for the whole
  product, and changed `llms.txt` the same way. #472 moved it back to reference
  coding with its measured 2.7x-fewer-output-tokens result at an equal solve
  rate, and added a community CTA. #478 then replaced the lead again with a
  paste-to-agent prompt — *"One prompt, and your AI agent installs XERJ, indexes
  your code, and reads the exact implementation"* — and removed the top-level
  `## Install` section, demoting installation to a `## Install by hand` section
  further down.

  So the net change versus rc.17 is the **#478** shape, not #472's, and rc.18
  does not lead install-first. The badge and body corrections from #457 survive
  all three (#457, #472, #478).

- **Capability badges corrected.** The default embedder is lexical and offline;
  neural is opt-in and downloads ~90 MB on first use. The previous pair
  (`semantic` beside `built-in, offline`) described a configuration that does
  not exist, and the README body said so correctly eighty lines below. The
  release badge no longer carries `include_prereleases&sort=semver`. Recording a
  correction to this entry rather than restating it: neither parameter is
  actually broken for this repository's tags — all four combinations, the
  pre-#455 URL included, render `v1.0.0-rc.17` against the live endpoint. The
  parameters were dropped as unnecessary, not as a fix (#455).

- **`autoindex --follow-symlinks` no longer follows a link out of the indexed
  folder unless you ask it to.** Pointing autoindex at a folder is not consent
  to index whatever that folder links to, and the hidden-file rule — the thing
  keeping `.env`, `.ssh` and `.aws` out of a queryable brain — was defeated
  entirely by a visible link name: `notes.txt -> .secretdir/k.txt` indexed the
  secret, and `shared -> /etc` indexed `/etc/shadow` under `shared/shadow`,
  while the run reported that the hidden directories had been pruned (#438).

  A followed link is now judged by what it **resolves to**, not by the name it
  wears. Targets outside the folder are refused and reported under a new
  `symlink:outside-root` rule; targets inside a hidden directory are skipped
  like any dotfile; and one that does both — left the folder AND resolved
  through a dotted component — is reported as `symlink:outside-root+hidden`,
  because it is refused even with the opt-in below and neither of the other two
  labels tells the operator anything they can act on.

  **This will index less than the previous release for some setups**, and the
  first run after upgrading drops what it previously indexed through such a
  link. If following links outward is why you turned the flag on — a vendored
  sibling checkout, a monorepo package link, a mounted volume — pass
  `--follow-symlinks-outside-root` to restore it (it requires
  `--follow-symlinks`, and is refused on its own rather than accepted as a
  silent no-op). That waives the root boundary
  and nothing else: the hidden-file rule still applies to the resolved path,
  judged from the point where the target diverges from the folder you pointed
  at — so `notes -> ../sibling/pkg` is followed and `keys -> ../sibling/.ssh/id_rsa`
  is not, while a dotted directory the two paths SHARE (a `/tmp/.tmpXXXX`, a
  checkout under `~/.local`) does not refuse anything, because your own folder
  is already inside it.

  Known limit, stated rather than implied: this is a rule about names and about
  where a link resolves. A **hard** link has neither, so a visible name hard-
  linked to a file inside a hidden directory is still indexed, on this release
  and on every earlier one.

### Fixed

- **A multi-index `scroll` reported the wrong `_index` on every continuation
  page, so `(_index, _id)` stopped being distinct**
  ([#414](https://github.com/xerj-org/xerj/issues/414)). The scroll context
  stored bare hits plus one context-level `index`, and every page after the first
  stamped that single name onto every hit. Page one was correct, because it maps
  the real per-hit index, so the divergence only appeared once paging crossed
  into the second index.

  The two context-creation sites stored *different* wrong values, which is why
  the symptom has two faces: `search_impl` (`/{a,b}/_search?scroll=`) stored
  `index_names.first()`, so hits were labelled with the first concrete index
  (`mi_a`), while `search_with_scroll` (`/{spec}/_search_scroll`) stored the raw
  path spec, so hits were labelled with the un-split string — the
  `_index: "sa,sb"` that #414 reported, 11,900 of 12,000 hits.

  Ids routinely collide across indices (each numbers its own documents), so a
  consumer keyed on `(_index, _id)` silently kept a fraction of the corpus.
  Measured on two 300-document indices with identical id ranges: **600 hits
  collapsed to 300 distinct `(_index, _id)` pairs**, all labelled with the first
  index. `search_after` over the same two indices was unaffected and returned
  600 of 600 — the two paging APIs disagreed about which index a document came
  from. The blast radius is reindex, migration, backup and CDC consumers, which
  are exactly the readers that use scroll and least survive losing half a
  corpus.

  The comment at the context-creation site asserted the opposite of the
  behaviour — that keeping the raw index spec left per-hit `_index`
  "authoritative when paging" — which is why the defect outlived review.

  `ScrollContext::hits` is now `Vec<(String, Hit)>`, so a hit carries the index
  it came from rather than borrowing one from the context. Both context-creation
  sites previously discarded it with the same `.map(|(_, h)| h.clone())`.

  **Scope, established by an independent adversarial verification of the fix and
  stated here rather than discovered later.** This covers scrolls addressed by a
  comma list (`/a,b/_search?scroll=`), a wildcard, or `_all`. It does **not**
  cover a scroll addressed through an **alias** spanning several indices: alias
  names are not expanded by the index resolver, so every hit still reports the
  alias and `(_index, _id)` remains non-distinct there. That matters because
  reindex and CDC tooling routinely scrolls an alias, which is exactly the
  audience named above — tracked separately rather than folded into this entry ([#433](https://github.com/xerj-org/xerj/issues/433)).
  The type change is also a breaking one for any out-of-tree reader of the
  public `ScrollContext::hits` field.

### Documentation

- **A three-model reference-coding benchmark, and a number that does not match the
  headline.** `demo/playbooks/REFCODING_BENCHMARK_3MODEL_2026-08-18.md` measures
  XERJ at **~1.4x fewer output tokens than grep**, consistent across all three
  models, plus a turnkey SWE-bench harness under `tools/xerj-code/swebench/`.
  The README, `llms.txt` and the badge headline **2.7x**, from the earlier
  single-model case study. Both are real measurements of different task sets;
  neither supersedes the other, and the 1.4x figure is the one measured across
  models. If you are choosing a number to quote, quote that one and say what it
  was measured on (#480).

- **Withdrew security and supply-chain claims the build does not back.** The
  security page's air-gap row moved from `GA` to `DOCS` and its
  `XERJ_AIRGAP=1 disables all telemetry` line was removed — that variable exists
  nowhere in the tree. The `SBOM · SUPPLY CHAIN` section, which promised a
  CycloneDX SBOM with every release, SLSA provenance, signed git tags and release
  archives, cosign-compatible signatures and reproducible builds, was replaced by
  a `RELEASE INTEGRITY` section stating what actually ships: a `.sha256`, which is
  a checksum and not a signature or an attestation. The public-sector page lost
  its `SBOM · SLSA` compliance row and one at-rest/BYOK line. Others on that page
  survive — two of them carrying no status marker at all — so the withdrawal is
  partial, not complete (#491).
  A verified air-gapped deployment recipe was added in their place (#430).


- **The 10,000-document scroll snapshot cap is now published, and pinned to the
  constant that enforces it**
  ([#370](https://github.com/xerj-org/xerj/issues/370)). XERJ's `_search?scroll=`
  is a bounded up-front snapshot, not a segment-walking cursor, so a query whose
  exact result set exceeds `SCROLL_SNAPSHOT_MAX_HITS` is refused with a 400
  rather than paged and silently truncated (#198). The refusal is the right
  behaviour and its message already names `search_after` as the way past it —
  but the ceiling appeared in no user-facing document, and scroll is precisely
  what the ES ecosystem reaches for to read a *whole* index
  (`elasticsearch-py`'s `helpers.scan()`, reindex internals, export and backup
  tooling), where the result set is normally larger than the cap. "Scroll is
  supported, with a 10,000-document snapshot cap; use `search_after` beyond
  that" is a different compatibility claim from "scroll is supported", and only
  the first one is true.

  The cap, the 400 it produces and the `search_after` alternative are now stated
  on `landing/docs/api-es-compat.html` (which also gains the three scroll
  endpoints it never listed), in the "what needs rewriting" list on
  `landing/docs/migration-from-es.html`, and in the honesty/capability-boundary
  section of `landing/llms-full.txt`. Every published figure sits in a
  `<!-- generated:scroll-snapshot-cap -->` region checked against
  `xerj_api::es_compat::SCROLL_SNAPSHOT_MAX_HITS` by
  `xerj-api/tests/scroll_snapshot_cap_is_documented.rs`, so changing the
  constant now fails the build until the pages follow — the mechanism already
  used for the capability counts in `docs_capability_lists.rs`. The same test
  drives the real ES-compat router over a corpus one document past the cap and
  reads the enforced number back out of the 400 the server produced, so the
  published number is checked against the server's behaviour rather than
  against a second hand-written copy of it.

  **The published escape hatch is now executed, not just spell-checked.** A
  pinned number is worth nothing if the recipe past it is wrong, and the first
  draft of this page's `search_after` transcript was: it paged with
  `search_after: ["999"]` against 11,450 numeric-looking `_id`s. `_id` is a
  keyword and sorts lexicographically, so `"1000" < "999"` and page 1 of 1,000
  actually ends at `_id` `"10897"`. Followed exactly as printed, including the
  page's own stop rule ("until a page comes back empty"), that walk collected
  1,010 of 11,450 documents — 10,440 missing, every response a 200 — which is
  the silently-truncated export that #198 and this very page exist to rule out.
  The transcript now publishes the cursor the response actually returns, shows
  the hit it comes from, and states the lexicographic ordering that produced the
  mistake. `published_search_after_recipe_walks_the_whole_corpus` parses the
  transcript out of the page, seeds the corpus the transcript itself names,
  replays the printed *paging* request bodies (the two carrying `sort`) against the real router, and
  asserts the walk collects every seeded `_id` over the page count the
  transcript claims — so a wrong cursor, an early stop, a repeated page or a
  drifted claim all fail the build.

  Three further scroll claims elsewhere in the tree were corrected to match:
  `landing/agent-search/index.html` said scroll *was* a deep-pagination cursor;
  `landing/llms-full.txt` §6 and `README.md` carried the bare "scroll is
  supported" form; and the ES→XERJ migration recipe in
  `docs/recipes/production-deployment.md` told readers they could point a
  whole-index scroll copy at a XERJ source with no mention of the ceiling.

  One boundary is documented rather than fixed here: the cap is applied to the
  request's summed total on `POST /{index}/_search?scroll=` (`es_compat.rs:14229`)
  but *per index* on the `POST /{index}/_search_scroll` alias
  (`es_compat.rs:19808`), so a multi-index scroll on that route can snapshot the
  ceiling from each index. That direction is permissive, not lossy. The pages now
  say so and point at
  [#405](https://github.com/xerj-org/xerj/issues/405), where the engine
  inconsistency is tracked.

  Verified by `cargo test -p xerj-api --test
  scroll_snapshot_cap_is_documented` — 3 passed in the `ci-test` profile
  (8.67s, overflow-checks off) and 3 passed in `dev` (12.40s, overflow-checks
  on). It drives `build_es_compat_router` over tempdir engines: a
  10,001-document index (`SCROLL_SNAPSHOT_MAX_HITS + 1`)
  refuses the scroll open with `400 illegal_argument_exception` naming `[10001]`
  and `[10000]`; a 128-document control scroll returns a `_scroll_id`; the
  printed first page returns 1,000 hits ending at `_id` `"10897"` with
  `sort: ["10897"]`; and the published walk from that cursor returns 11,450
  distinct `_id`s over 12 pages with none missing and none repeated. Reverting
  only the transcript hunk to the `["999"]` form fails the same test with
  `left: Array [String("999")] / right: Array [String("10897")]`, and with the
  cursor assertion inverted so the walk still runs, with "collected 1010 of
  11450 documents over 2 pages, silently missing 10440". The `_search_scroll`
  divergence above was measured on two 6,000-document indices in one engine:
  `POST /sa,sb/_search?scroll=1m` → 400 naming `[12000]`/`[10000]`;
  `POST /sa,sb/_search_scroll?scroll=1m` → 200 with a `_scroll_id` and
  `hits.total {value: 12000, relation: "eq"}`.

### Fixed

- **`xerj autoindex` now skips hidden names that are not valid UTF-8.**
  The walker already refused `.env`, `.ssh`, and `.git`, but the check
  used `file_name().to_str().starts_with('.')`. On Unix a name whose
  first byte is `.` and that is not valid UTF-8 (`to_str()` is `None`)
  was walked and indexed. The skip now compares the first encoded byte
  to `.`. Typical UTF-8 secrets were already skipped. `--dry-run`'s
  `ignored_files_in_pruned_dirs` count uses the same predicate, so a
  hidden non-UTF-8 file (or the contents of a hidden non-UTF-8
  directory) is no longer reported as a non-hidden file inside a
  pruned directory.

  **Upgrade.** A corpus indexed by a pre-fix binary that contains a
  hidden non-UTF-8 name looks like a file removal to the graph-enabled
  path (the default, and what `xerj brain` uses). As of
  [#439](https://github.com/xerj-org/xerj/issues/439)'s first fix the
  rerun's refusal names the real cause — the file is still on disk,
  excluded, not removed — instead of the misleading "restore the removed
  file(s)". It still exits 1 with `unsupported_content_group_removal`,
  writes nothing, and the already-indexed documents still stay live;
  sweeping them (so a widened exclusion removes the data) is tracked in
  [#589](https://github.com/xerj-org/xerj/issues/589). Recovery that
  removes the data today: delete the published indices and the state
  directory, then re-index the folder. An isolated rebuild with a new
  `--state-dir` / `--prefix` also exits 0, but leaves the old target's
  documents live until those indices are deleted by hand.

- **Autoindex snapshot and replay one-shot failpoints can no longer be stolen by unrelated parallel tests.** ([#385](https://github.com/xerj-org/xerj/issues/385)).
  The two test-only failpoints are now owned by the thread that arms them, while
  retaining their existing one-shot behavior and serialization locks.

- **Console delete/upsert no longer reports success after a refused write.**
  `DELETE /_xerj-console/api/v1/views/{id}` swallowed `delete_document`
  errors and returned `204` while the view stayed on disk (the same class
  as the passkey revoke fix). Auth and prefs writes used delete-then-create,
  so a replacement the engine rejected (write block, unparseable date)
  dropped the only live user, session, token, or prefs row. Deletes now
  propagate the engine error; upserts use a single `index_document`.

- **`sort` on an ES meta-field other than `_score` / `_doc` / `_id` resolved to
  `null` on every hit** ([#401](https://github.com/xerj-org/xerj/issues/401)).
  `compute_sort_values` special-cased exactly those three keys and sent
  everything else to `get_field_value(source, field)` — a lookup *inside*
  `_source`. `_seq_no`, `_version`, `_primary_term` and `_index` are engine
  metadata and are never literal `_source` keys, so the lookup missed on every
  document and the whole result set tied on `[null]`. The damage lands on
  keyset pagination: `search_after: [null]` is never strictly greater than the
  next hit's `[null]`, so `sort: [{"_seq_no":"asc"}]` + `search_after` — the
  consistent full-scan pattern used for migrations — either re-reads page one
  forever or stops after it, with no error either way. Measured on a 30-document
  index before the fix: 4 ids collected out of 30, page two empty.

  These fields now resolve through the same version-map accessors that populate
  the response meta-fields (`lookup_seq_no` / `lookup_version`), so a hit's
  `sort` value and its `_seq_no` / `_version` agree. `_primary_term` is the
  constant `1` the hit meta-field already emits and `_index` is the index name;
  both are constant within one index, so they tie like any other constant sort
  key and a client paginating on them still needs a tie-breaker. Sorting on
  `_seq_no` under `index.disable_sequence_numbers` is unchanged — es_compat
  rejects it at the API edge before the request reaches the engine.

  Real sort values are only half of it: the memtable's bounded sort-candidate
  path narrows the heap's input using a doc-values column keyed by the sort
  field, which for a meta-field is derived from `_source` rather than from the
  version map the heap now ranks on. With no such key present it classified
  every buffered document as "missing the field" and returned an arbitrary
  `materialisation_limit`-sized prefix. A correctly ranked page over a wrong
  candidate set is still a silently wrong page, so that path — and the
  segment-side sort shadow — now decline meta-fields and take the full walk.
  The same gate fixes an independent instance of the identical defect on `_id`,
  which already resolved to a real sort value: over a 2 000-document memtable
  `sort: [{"_id":"desc"}]` returned
  `["d1995","d1989","d1981","d1973","d1961"]` instead of
  `["d1999","d1998","d1997","d1996","d1995"]`.

  There is a third such prefilter, and it is the one the two gates above route
  meta-sorts *onto*: the memtable scan's pre-clone rejection
  (`memtable_primary_key_rejects`) skips a document whose primary sort key,
  read from `_source`, already loses to the full heap. Once the heap ranks on
  version-map metadata, a document carrying a `_source` key of the same name
  is compared against the wrong frontier and dropped before the heap ever sees
  it. Measured over ES-compat HTTP on 2 000 documents where only `d0000`
  carried `"_seq_no": 10000000`: `{"size":5,"sort":[{"_seq_no":"asc"}]}`
  returned `[d0001, d0002, d0003, d0004, d0005]`, omitting the true first
  document, while `"size": 3000` returned it — a silent, size-dependent
  missing document. That path now declines meta-fields too.

  **Behaviour change:** `sort` on `_seq_no` / `_version` / `_primary_term` /
  `_index` now ignores a `_source` key of the same name and always ranks on
  engine metadata. Previously it ranked on the `_source` value where one
  existed and on `null` everywhere else. This is observable because xerj —
  unlike Elasticsearch, which rejects a metadata-named key inside a document
  with a `document_parsing_exception` — accepts `{"_seq_no": 1}` in a document
  body and echoes it back in `_source`. Such a key is now inert for sorting;
  it is still stored, still returned in `_source`, and still usable for
  ordinary queries. Rejecting metadata-named `_source` keys at the API edge is
  deliberately **not** part of this change — xerj writes `_routing` into
  `_source` itself, so the edge rule needs its own design.

- **`xerj autoindex` aborted a whole run when one file declared two or more SQL
  tables** ([#360](https://github.com/xerj-org/xerj/issues/360)). One file can
  feed several datasets — a SQL dump is one file and N tables — but the sealed
  record count was a single whole-file total. Finalisation charged that total to
  *each* of the file's datasets in turn and compared it against that one
  dataset's read-back, so any multi-table file disagreed with itself by
  construction and exited `1` with `dataset t1 exact read-back count 2 disagrees
  with sealed count 4`. The rows were always indexed correctly; only the
  reconciliation arithmetic was wrong, so `exit=1` discarded a complete corpus.
  Reported against a real `unum-cloud/usearch` file — `sqlite/README.md`,
  ordinary prose with three ` ```sql ` blocks in it, which the sniffer routes to
  the `sqldump` family.

  The sealed ledger is now keyed by the dataset identity each record was written
  under (`records_by_dataset` on the prepared artifact,
  `expected_records_by_dataset` on the manifest group), the read-back is scoped
  by `ax_dataset` as well as `ax_file`, and `validate_groups` rejects a ledger
  that names a dataset the group does not feed or that does not sum to the group
  total. Measured against a local node with the fixed binary: the two-table and
  three-table repros from the issue exit `0`, every table's rows are present
  (`_count` 2 per table), and each dataset's catalog document reports
  `record_count: 2` instead of the whole file's total.

  Existing state directories survive the upgrade: both new fields are omitted
  from serialization when empty, so a snapshot or manifest sealed by an older
  version re-serializes to the same bytes and keeps its digest. A pre-upgrade
  group that fanned out has no recoverable split, so it withdraws from that one
  equality rather than failing it — the state directory left behind by the
  aborted repro run resumes and commits on the fixed binary.

- **`xerj autoindex` aborted a run because *another* corpus on the same node had
  already indexed a byte-identical file** — the second of the three aborts
  [#360](https://github.com/xerj-org/xerj/issues/360) reports, and the one that
  killed the reporter's second corpus. A file's identity is derived from its
  CONTENT alone (`content::full_digest`) and the catalog is one global index
  (`autoindex-catalog`) that no `--prefix` namespaces, so two unrelated
  checkouts sharing one file — an Apache-2.0 `LICENSE` is the everyday case —
  share that file's catalog document IDs. A corpus holding the content five
  times publishes a canonical document plus four alias documents; a corpus
  holding it once republishes only the canonical, and the other corpus's four
  alias documents survive under their own `run_id`. Be clear about what that
  does *not* mean: the shared file's canonical catalog document
  (`file:<content_id>`) belongs to whichever run published it last, so after the
  second corpus runs, the first corpus's alias documents point at a
  `duplicate_of` whose canonical entry is now the other corpus's. That is
  pre-existing — the binary before this change overwrites it too, then aborts —
  and this change removes the false abort without repairing it. The catalog
  identity collision itself is tracked in
  [#416](https://github.com/xerj-org/xerj/issues/416). The generation-wide barrier
  counted every catalog document carrying the group's `file_key` — across every
  run on the node — against `1 + aliases.len()` for the group in front of it, so
  the second run exited `1` with `catalog canonical/alias count disagrees with
  desired group axg1-…` on a corpus whose data was complete. The count is now
  scoped to this generation's `run_id`, the same way the run-summary read-back
  already is, so the barrier asks what it exists to ask: did *this* generation
  publish one canonical document and its aliases.

  Measured on the reporter's own corpora, one node, release binary,
  `aarch64-unknown-linux-gnu`. `apache/lucene` and `unum-cloud/usearch` share
  exactly one indexable content identity — the 11,357-byte Apache-2.0 `LICENSE`,
  present 5× in lucene and 1× in usearch. Indexing lucene and then usearch on
  one node, each with its own `--prefix` and `--state-dir`: **before**, usearch
  exited `1` after 41.5s with `catalog canonical/alias count disagrees with
  desired group axg1-f5c714bcb8c945dd3687e83218352c66`. **After**, the same
  usearch run prints `generation 1 committed — 4 datasets, 308 records live` and
  exits `3` (junk recorded, never fatal — `cli.rs` EXIT CODES) in 30.2s, and the
  43 alias documents lucene published are still in the catalog: the barrier
  stopped counting them, it does not delete them.

  This is not what closed #360 — that closed `completed` on 2026-08-15 via #407. The third abort the issue reports — `catalog
  read-back for ds:… disagrees with the sealed generation projection`, which
  ended the full `apache/lucene` run after 1,073s before this change and 663s
  after it — reproduces on this branch and is not an autoindex defect:
  `POST /_bulk` drops explicit `null` fields from `_source` once the request is
  large enough (measured directly with `curl` against this binary: 1,500
  documents / 129,786 B keeps them, 4,000 documents / 349,786 B loses them,
  2,000 documents is where it starts to be mixed), and autoindex's dataset
  documents carry `"time_field": null`, `"time_min": null`, `"time_max": null`
  and `"semantic_field": null`. The exact read-back is the tripwire that caught
  the engine rewriting a document, so it is deliberately left armed rather than
  taught to ignore the difference. Tracked as
  [#415](https://github.com/xerj-org/xerj/issues/415).

- **A lexical query on a `semantic_text` field now says so** (#363). `match`,
  `match_phrase`, `multi_match` and `query_string` against a `semantic_text`
  field score with BM25 over the analysed text and never consult the embedding
  that field was indexed to produce — the response was byte-identical to the
  same query against a plain `text` field, same ids and same scores, so a
  caller had no way to tell which question had been answered. `_search`
  responses now carry an additive `_xerj.hints` entry (`code:
  "lexical_on_semantic_text"`) that names the field, says the embedding was
  not consulted, and gives a ready-to-paste `semantic` query body. Scoring,
  hits and every ES-compat response field are unchanged, and the hint stays
  quiet when the same request already reaches the vector through `semantic`
  or `knn` (including the ES 8.x top-level `knn` block).
- **A `quantization: "scalar8"` (`int8_hnsw`) vector field scored updated
  documents from stale quantized data, silently, forever**
  ([#371](https://github.com/xerj-org/xerj/issues/371)).
  There were **two** write-once caches on this path, and either one alone
  reproduces the reported symptom.

  1. The per-field SQ8 **code store** was populated write-once: the first kNN
     that observed a document computed its codes and nothing ever recomputed
     them — not an update, not a delete-and-reindex, not a merge.
  2. The per-field **codebook** (`Sq8Params`, the per-dimension min/scale pair)
     was fitted from the first <=1000 candidate vectors the field was ever
     scanned with and then kept for the life of the process. A vector written
     afterwards that falls outside that fitted range is *clamped* into it, and
     when the fitted range is narrow the clamped decode is indistinguishable
     from the vector the document used to hold.

  Both were reproduced at the HTTP boundary before the fix, each against a
  byte-identical full-precision control index that performs the identical
  manipulation:

  * **codes** — 8-dimension cosine field, doc `0` starts as the query vector
    and is rewritten with its exact negation. The control drops it as it must;
    the `scalar8` index returned it at `_score` **0.99999774**, the same value
    to the last bit as before the rewrite, with the entire 20-document score
    list unchanged, while `GET /{index}/_doc/0` confirmed `_source` had taken
    the negation. `PUT /{index}/_doc/{id}` and `_bulk` were both affected;
    `_bulk` was untested in the original report.
  * **codebook** — same shape, but over a corpus whose dimension 0 never leaves
    `+1.0`, so it fits the degenerate range `[1,1]`. Doc `0` negated to `-1.0`
    decodes straight back to `+1.0`: still first at `_score` **1.000000**, the
    whole score list frozen. On the workload the issue names as what makes it a
    blocker — re-embedding a corpus after a model change — a codebook fitted to
    the old embedding space clamped 50 re-embedded documents into a 0.002-wide
    score band, destroying ranking outright.

  The fix removes both caches rather than adding invalidation to them. The
  brute-force scan already holds each candidate's live f32 vector — it just
  read it out of `_source` to apply the filter — so it now fits the codebook
  over exactly the candidates it is about to score and quantizes/dequantizes
  each one on the spot, through a new `Sq8Params::encode_into` and
  `Sq8Params::fit_borrowed`, into two buffers reused across the scan (no
  per-document allocation). **No SQ8 state of any kind now outlives a query**,
  so there is nothing left to go stale. Fitting over exactly the set being
  encoded additionally makes clamping impossible on this path rather than
  merely unlikely: every value passed to `encode_into` is inside `[min,max]` by
  construction. `scalar8` still scores from 1-byte-per-dimension codes, so its
  recall profile is unchanged — measured recall@10 **0.998** against the exact
  float32 index on the `vector-quantization` recipe corpus, byte-identical
  scores to before this change on that (unfiltered, <1000-doc, never-updated)
  shape.

  Three defects in the removed objects went with them: unbounded growth (the
  code map had no eviction and no upper bound — one entry per document ever
  queried, for the life of the process), an exclusive write lock taken on the
  query path on *every* kNN over the field, and a codebook that could be fitted
  from an empty candidate set — a single query with a wrong-length vector
  installed an all-zero-scale codec that every later score on that field
  collapsed onto. That last one is now structurally absent rather than guarded:
  a codebook that does not outlive its query cannot be observed by another.

  **Not closed by this change**, tracked in
  [#392](https://github.com/xerj-org/xerj/issues/392):
  * `scalar8` still reads the full-precision vector from `_source` on every
    query, so it **does not reduce resident memory** — it buys the precision
    profile of int8 and nothing else. Making it a memory win means the
    ordinal-addressed, ingest-time code array described in #371, written next
    to the data it describes as Lucene does it.

    The "~4x smaller vector working set" wording has been corrected in every
    place this change could find it, listed exhaustively so the claim is
    checkable rather than taken on trust. **Docs and shipped artifacts:**
    `docs/recipes/vector-quantization.md`, `docs/recipes/README.md`,
    `docs/examples/vector-quantization/quant_demo.py`,
    `recipes/vector_quantization.py`, `engine/xerj.default.toml`,
    `xerj-common/src/config.rs`, `xerj-common/src/types.rs`,
    `xerj-vector/src/quantizer.rs`, `demo/playbooks/ES_COMPATIBILITY.md`,
    `demo/playbooks/STUB_AUDIT.md`,
    `demo/playbooks/USER_FEEDBACK_SCORECARD_RC4.md`. **Published site**
    (`landing/docs/*.html` is the hand-maintained mirror of `docs/*.md`):
    `landing/docs/recipes/vector-quantization.html`,
    `landing/docs/recipes/index.html`, `landing/docs/index.html`,
    `landing/docs/vectors.html`, `landing/docs/config.html` (both the
    reference table and the example TOML), `landing/docs/operations.html` (the
    RAM capacity-planning line), `landing/docs/playbooks/vector-search.html`,
    plus the two `docs-index` search snippets that are duplicated into all 44
    docs pages. **Marketing copy:** `landing/solutions/index.html`,
    `landing/pricing/index.html`, `landing/resources/index.html`,
    `landing/industries/finserv.html`, `landing/agent-search/index.html`,
    `landing/use-cases/ai-search-retrieval.html`, `landing/product.html`,
    `landing/llms-full.txt`.

    **Deliberately NOT corrected here**, because replacing them needs numbers
    this change cannot supply: the interactive capacity model on
    `landing/industries/retail.html` (lines 82/93/177) and the matching row on
    `landing/industries/healthcare.html:188` size a deployment from an SQ8
    vector footprint (e.g. "18 GB (SQ8)" vs "92 GB (float32)"), as does the
    "10 M SKUs ON ONE NODE · SQ8" card at `landing/industries/index.html:123`.
    Those are product claims with a whole page built on them and they are
    wrong until #392 lands; they need replacement figures and sign-off, not a
    silent edit. Also left as-is: the dated review record
    `engine/reports/FEATURE_FAIRNESS_REVIEW_v0.6.0_2026-04-25.md` (lines
    81/272), which is an archive of what was believed on 2026-04-25, and the
    synthetic demo corpus article "Scalar quantization: 4x memory savings for
    free" (`demo/data/generate_ai_kb.py:56`, echoed as sample search output at
    `demo/DEMO_RUNBOOK.md:528`), which is generic industry text in a sample
    knowledge base rather than a claim about XERJ.
  * because the codebook is now fitted per query, a `scalar8` `_score` depends
    on the candidate set — **and that changes the returned order, not merely
    the score.** Each individual score moves by at most SQ8's own quantization
    step (1/255 of the fitted per-dimension range), inside the approximation
    error `scalar8` already carries, but documents whose true scores are close
    swap places, so a caller sees a different ranking. Measured at the HTTP
    boundary against this branch, on a 60-document 4-dimension cosine field:

    - adding `filter: {"term": {"grp": "a"}}`, which removes only the 30
      unrelated `grp:b` documents, returned the same 30 survivors with a
      maximum `_score` difference of **1.976e-05** but a **different order at
      19 of the 30 positions**;
    - the trigger is the candidate set, not the `filter` keyword — indexing
      **one** more unrelated document moved the same 30 by up to **7.100e-06**
      and reordered their top 10;
    - on an **unfiltered corpus over 1000 documents** the scores also differ
      from v1.0.0-rc.17, which fitted the codebook from the first ≤1000
      candidates and cached it for the life of the process. On 1500 documents
      all 40 of the top-40 `_score` values changed (maximum **4.880e-05**) and
      two adjacent ranks swapped. This is a wire-visible change on any
      `scalar8` field with more than 1000 candidates. Below 1000 documents,
      unfiltered and never updated, scores are byte-identical to rc.17 — that
      is the shape the recipe corpus has, and the only shape the
      "byte-identical scores" note above covers.

    None of this could happen on rc.17 once the first query pinned the
    codebook, so it is behaviour this change introduces; it is the price of
    the codebook not outliving the query. It is also **not** the dependency
    Lucene has: Lucene fits per segment at index time, so a Lucene score is a
    function of the index state alone, while here it is a function of the
    query's candidate set as well.
  * `scalar8` continues to disqualify a field from HNSW-served kNN, so opting
    in still costs ANN.

  Cost: measured, but **not resolved**. `ci-test` profile, 4000 docs x 384
  dims, warm, min/p10/p50 of 60 requests, three builds per side. On the
  scan-bound shape (`k=4000`) the two sides overlap and the ordering flips with
  the statistic — best min-of-60 was 22.66 ms on main vs 24.88 ms here, but
  best p10 was 29.26 ms on main vs 25.09 ms here — on a machine that produced
  p50 outliers up to 159 ms from background load. No direction can be claimed
  for that shape from these numbers. The small-`k` shape (`k=10`,
  `num_candidates=100`) was consistently marginally faster here, 0.15-0.16 ms
  against 0.17-0.18 ms on main, which is the per-query `RwLock` read on the
  codec store going away. In principle this change adds one min/max pass over
  the candidate vectors and removes a lock acquisition; both are small against
  the encode/decode/similarity work the scan already does.

- **`_count` no longer reports documents `_search` cannot return**
  ([#362](https://github.com/xerj-org/xerj/issues/362)). `POST /{index}/_count`
  is `_search` at `size: 0`, so the two are supposed to answer the same
  question. On a `text` field they did not. A `text` mapping carries no
  doc-values, so the term-count shortcut had no column to read and answered
  from the segment's FTS term dictionary instead — analysed tokens, tokenised
  and lowercased — while `_search` resolves a `term` on such a field against
  the whole `_source` value. Two oracles, two questions, wrong in **both**
  directions on a flushed segment:

  | `term` query on a `text` field | old `_count` | `_search` |
  |---|---|---|
  | `{"term":{"title":"testsegmentreader.java"}}` | `1` | 0 hits |
  | `{"term":{"title":"quick"}}`, value `"the quick brown fox"` | `1` | 0 hits |
  | `{"term":{"title":"Doc7.java the quick brown fox"}}` — the byte-exact `_source` value | `0` | **1 hit** |

  The shortcut now abandons when a segment has no doc-values column for the
  field, and the ordinary delete-aware scan — the code that produces the hits —
  answers. That shortcut is not `_count`-only: its result also authorises the
  bounded stored scan at `size > 0`, so `hits.total` was affected too. On a
  200,000-document index, with a `term` on a `text` field matching 10,000
  documents, `_search` at `size: 10` returned its 10 hits under
  `"total": {"value": 130}`, and `_count` for the same query answered `0`. Both
  now answer `10000`.

  **This costs real time, and the bill is not only "the wrong spellings".**
  Scan cost does not depend on how the term is spelled, so every `term` count on
  a `text` field with no doc-values now pays a full scan — the byte-exact
  spelling included. Its cost changes exactly as much as a misspelling's does,
  and per the table above its *value* changes too, from an undercount to the
  truth. Measured on this branch in the `ci-test` profile, one segment, 20
  distinct query values per shape to defeat the request cache (median of 20,
  same box, same corpus, only `index.rs` swapped):

  | `_count` shape, 200,000 docs | before | after |
  |---|---|---|
  | `term` on `text`, no doc-values | 4.2 ms | 356 ms |
  | same, byte-exact `_source` value | 4.1 ms (and answered `0`) | 356 ms (answers `1`) |
  | `term` on `keyword`, `doc_values: false` | 21 µs | 45 µs |
  | `term` on `keyword` with doc-values (control) | 347 µs | 216 µs |
  | `term` on `long`, `doc_values: false` | 383 ms | 353 ms |
  | `_search` at `size: 10`, `term` on `text` matching 10,000 docs | 9.4 ms (total `130`) | 225 ms (total `10000`) |

  At 50,000 documents the first row is 2.3 ms → 81 ms. That is roughly 35× at
  50 k and 85× at 200 k, but the box carried load average 70–85 from unrelated
  work throughout, so treat the multiple as order-of-magnitude; the number to
  plan around is the absolute one, about a third of a second per `_count` at
  200,000 documents.

  The middle rows are there to bound the blast radius, and they are the
  reassuring ones. A `keyword` field with `doc_values: false` reaches the same
  abandoned branch, but a `term` on a declared `keyword` field still projects
  into the FTS tree, so the segment scan's own `term_doc_freq` shortcut answers
  it: its **route** changed, its cost class did not, and it stays sub-linear
  rather than becoming a scan. A `long` with `doc_values: false` is unchanged
  because it was already scanning — a field with no FTS dictionary never reached
  the removed fallback in the first place. And every count already served from a
  doc-values column is untouched, which is what the control row shows.

  **One shape's `_search` page changes, not just its total.** With the shortcut
  gone the stored scan no longer stops after materialising `from + size`; it
  walks every match and then picks the page. For an *unsorted* `term` on a
  `text` field with many equally-scoring matches, that changes which documents
  come back — measured over 2,000 identically-scoring documents at `size: 10`,
  the ten ids differ before and after (and the total goes from `80` to `2000`).
  Every score involved is identical, so neither page is more correct than the
  other, but the change is observable. Queries with an explicit `sort` return
  byte-identical pages and identical totals before and after.

  **ES compatibility, stated rather than buried.** On the repaired shape this
  moves `_count` *away* from the Elasticsearch answer while making it
  self-consistent: ES answers `{"term":{"body":"quick"}}` with `1` against
  `"the quick brown fox"`, XERJ used to answer `1` as well — but with zero
  retrievable hits — and now answers `0`, which is what its own `_search`
  returns. A count no page of hits can reproduce is a coincidence, not
  compatibility. What `_search` *should* answer for a `term` on an analysed
  field is a separate, hit-set-moving decision. It was raised as
  [#397](https://github.com/xerj-org/xerj/issues/397), which was closed
  `not_planned` on 2026-08-15 — not abandoned, but consolidated into
  [#423](https://github.com/xerj-org/xerj/issues/423), which is open and carries
  #397's repro as acceptance criteria. #423 is the tracker to watch: it treats
  the cause rather than this symptom — `doc_matches_query` evaluates buffered
  documents against raw `_source` with no mapping and no analyser, while flushed
  documents go through the term dictionary. Until it closes, this shape has no
  sub-linear count: budget for the measured cost above, or avoid `_count` with a
  `term` on an analysed field in a serving path.

  Two things this change does **not** do. It does not touch the other half of
  #423, which consolidated #362 (closed `not_planned` for that reason) — `case_insensitive` accepted and silently dropped on
  `prefix`/`wildcard`, the term/prefix case-folding split #362 reported:
  `{"term":{"title":"TestSegmentReader.java"}}` still answers `1` while
  `{"prefix":{"title":"TestSeg"}}` answers `0`. And it is not the narrower fix
  the issue thread proposed (dropping the lowercase retry the shortcut did on a
  dictionary miss); that was tried and measured, and it leaves the first two
  rows of the first table wrong while additionally making the byte-exact
  `{"term":{"title":"TestSegmentReader.java"}}` count `0` where `_search`
  returns `1`.

## [1.0.0-rc.17] - 2026-08-15

### Added

- **`xerj autoindex` understands Unity projects.** Text-serialized scenes,
  prefabs, `.asset`/`.mat`/`.anim` files become **one record per
  GameObject/Component** (`unity_class`, `unity_class_id`, `file_id`,
  `ref_guids`, `script_guid`, plus the flattened body); `.meta` sidecars become
  a guid↔asset-path table; MonoBehaviour records carry a denormalized
  `script_class`/`script_path` so "which scenes use this script?" is one query.
  Detection is by the `%YAML` + `%TAG !u! tag:unity3d.com` header and never by
  extension, so binary-serialized assets stay junk (enable Force Text
  serialization). Unity's generated directories (`Library/`, `Temp/`, `obj/`,
  `Logs/`, `UserSettings/`) are pruned and recorded only when a sibling
  `ProjectSettings/ProjectVersion.txt` proves the tree really is a Unity
  project.

  Reland of community PR #274 by **@gonchar**, brought up to current `main` and
  corrected — see Fixed below for what changed on the way in.

- **BVH motion capture** — one metadata record per clip (`joints`,
  `joint_count`, `frames`, `frame_time_s`, `duration_s`). The numeric MOTION
  block, which is most of the file, is never read: extraction stops at the
  `Frame Time:` line.

- **`--stub <glob>`** designates files that should be *referenceable but not
  parsed*: each match is indexed as one name-card record and its contents are
  never opened.

- **Push a filtered subset of indices to an external ES-compatible target —
  the single-node WAL tap** ([#320](https://github.com/xerj-org/xerj/issues/320)).
  Nothing pushed data out of the engine before this: `_ccr/*` answered `501`,
  reindex-from-remote was refused up front, and snapshot/restore — scheduled and
  whole-index — was the only export path. A XERJ node used as a lightweight edge
  collector had no way to stream a curated slice of its data up to a central
  cluster.
  - New `[wal_tap]` config section (10 settings, off by default) and a runtime
    REST surface: `GET`/`PUT`/`DELETE /_xerj/wal_tap` and
    `GET /_xerj/wal_tap/_stats`. The allowlist is glob-based and adjustable
    without a restart. That brings the documented config surface to
    **115 settings** (counted by `journey_zero_config`, not by hand).
  - **Every `/_xerj/*` route is superuser-only.** `PUT /_xerj/wal_tap` is a
    whole-node export primitive — `target_url` names any host, `indices: ["*"]`
    names every index, and the tap attaches an operator-supplied
    `Authorization` header — so it is data exfiltration and authenticated SSRF
    in one call. Same reasoning `authz.rs` already applies to snapshot and
    restore: nothing here is index-scoped, so there is no per-index decision to
    make. The reads are covered too — `GET /_xerj/wal_tap` echoes `target_url`
    and `_stats` enumerates the node's whole index inventory.
  - **`target_url` may not carry credentials.** `https://user:pass@host` is an
    ordinary URL that `reqwest` turns into a `Basic Authorization` header — i.e.
    `target_auth` by another spelling, in the one field the API echoes back and
    the boot log prints. It is refused at startup and by `PUT`, and redacted
    everywhere it is rendered.
  - The wire format is `_bulk` and nothing else, so the target can be
    Elasticsearch, OpenSearch, or another XERJ node with nothing installed.
  - **This is not CCR.** It is one-directional, single-node, and independent of
    the post-GA sharding/replication track. `_ccr/*` still answers `501`.
  - The tap adds nothing to the write path: it tails WAL files from disk on a
    timer and takes no lock the writer wants. A poll costs the bytes appended
    since the last one, not the size of the file — the tail reader is a sliding
    window over a `File`, shaped after Lucene's `BufferedIndexInput.refill`
    (`lucene/core/src/java/org/apache/lucene/store/BufferedIndexInput.java:289-317`,
    Apache-2.0). A caught-up poll reads the 16-byte WAL header and stops —
    measured at exactly 16 bytes over a 4,176,200-byte generation, asserted
    through `tail_shard` itself by
    `wal::tests::a_caught_up_poll_does_not_read_the_whole_generation`. (The
    window's 64 KiB read-ahead is deliberately not applied to that header read:
    it was, and it made the figure 65,536 bytes per shard per poll rather
    than 16.)
  - Delivery is at-least-once, with `version_type: external` carrying the WAL
    `seq_no` on every action so a redelivery converges on the same document the
    source holds. Covered against a stub that implements the actual ES external
    versioning rule — absent `_id` is created at any version, present `_id` is
    overwritten only by a strictly greater one, everything else comes back
    `409 version_conflict_engine_exception` — and asserted on both halves: four
    documents accepted on the first delivery, four conflicts and zero extra
    `docs_shipped` on the redelivery, watermark unchanged. Note that XERJ's own
    `_bulk` ignores per-action `version` / `version_type` (only the single-doc
    path honours them), so a XERJ target degrades to last-write-wins by
    arrival; against Elasticsearch or OpenSearch the mechanism is live.
  - `_stats` counts what the **target accepted**, per item, not what was
    rendered into the request. `lag_seq` is derived from the highest accepted
    `seq_no`, and each index carries a `healthy` boolean that is false both when
    sends are failing and when the target answers `200` while applying none of
    the actions inside it — for **any** reason, not only a version conflict.
    A target rejecting every document with `mapper_parsing_exception` is a `200`
    with `errors: true`, so no send fails and the cursor advances past every
    dropped document; `last_item_rejection` in `_stats` carries the target's own
    word for it, because "fix the mapping" and "look for a second writer" are
    opposite responses.
  - Cursor state is flushed once per poll cycle rather than once per index per
    poll; a deferred flush can only cause a redelivery, which the external
    versioning above absorbs.
  - Deleting an index drops its cursor at the moment of deletion, so a
    `DELETE` + `PUT` of the same name inside one poll interval — never observed
    as an absence at a 500 ms default — cannot leave a byte offset pointing into
    a WAL stream that no longer exists. For a delete that happened while the
    node was down, the reader treats a cursor offset past the end of its
    generation as the discontinuity it is (WAL files are append-only) and
    restarts the stream with a reported gap instead of clamping to EOF.
  - WAL retention is deliberately *not* coupled to the tap — a slow remote must
    not be able to fill the local disk. A tap that falls far enough behind loses
    entries and says so: `gaps` in `_stats`, plus a warning per occurrence. New
    `wal_tap.min_retained_generations` (default `0`, unchanged behaviour) buys
    **bounded** slack: it is a floor, not an Elasticsearch-style retention
    lease, so it holds at most `n × storage.wal_max_size_mb` per WAL shard
    however far behind any consumer is. The floor is enforced inside
    `IndexStore::wal_maintain_all_verified` — the prune pass the engine actually
    runs — using the same arithmetic and the same "inside the deletion pass, not
    in its callers" placement as Lucene's
    `KeepLastNCommitsDeletionPolicy.onCommit`
    (`lucene/core/src/java/org/apache/lucene/index/KeepLastNCommitsDeletionPolicy.java:51-58`,
    Apache-2.0). At the default `0` the loop is unchanged: no extra syscall, no
    behaviour change.
  - **The retention floor is live, and the API no longer promises otherwise.**
    `min_retained_generations` is the one `[wal_tap]` setting that does not live
    in the tap — it lives in every open index's `WalWriter`, seeded at open from
    `Engine.config`, an `Arc<Config>` written once at boot and never mutated.
    `PUT /_xerj/wal_tap` therefore pushes it straight onto the live writers
    (`Engine::apply_wal_retention_floor`, after Lucene's `LiveIndexWriterConfig`
    and `IndexFileDeleter.revisitPolicy`,
    `lucene/core/src/java/org/apache/lucene/index/LiveIndexWriterConfig.java:39-126`
    and `IndexFileDeleter.java:516-543`) and answers with
    `retention_floor_applied_to_indices` — the number of writers it reached —
    instead of a warning. `DELETE /_xerj/wal_tap` releases it the same way, and
    `Engine::new` folds the tap's effective configuration into the boot `Config`
    so a store opened after a restart is seeded with the persisted floor rather
    than the file's. Verified over HTTP end to end
    (`xerj-api/tests/wal_tap_retention_floor_is_applied.rs`): after
    `PUT {"min_retained_generations": 3}` an index opened before the call reads
    `3` on all 16 WAL shards, an index created after it opens with `3`, and both
    still read `3` after a restart whose config file still says `0`.
  - A configuration set through `PUT /_xerj/wal_tap` is persisted next to the
    cursors and re-applied over the config file on the next boot, so "runtime
    config, no restart" does not quietly mean "and gone after one".
    `DELETE /_xerj/wal_tap` drops the overlay and reverts to the file — to the
    value the tap kept verbatim at construction, not to a re-read of
    `Engine.config`. The state file holds `target_auth`, so it is written `0600`
    through the same writer the API-key store uses.
  - The unapplied-batch health signal counts **polls**, not `_bulk` requests.
    How many chunks a poll splits into is `max_batch_bytes` arithmetic: at
    `max_batch_bytes = 1` one legitimate at-least-once redelivery of four
    documents produced four all-conflict `_bulk` responses, tripped the
    three-in-a-row threshold on its own, and emitted `healthy: false` with
    `last_error: "… nothing is being replicated"` about a poll in which every
    document was already at the target. The field is now
    `consecutive_unapplied_polls` and rises by at most one per poll; three
    unapplied polls in a row still reports the stall.
  - Every multiply in the retry-backoff computation saturates.
    `max_retry_backoff_secs.max(1) * 1000` overflowed above ~1.8e16: a panic
    under `cargo test`'s overflow-checks that killed the spawned tap task
    silently, and under a release profile a *wrapped* cap that can be
    arbitrarily small — the value `18_446_744_073_709_552` wrapped to a 384 ms
    ceiling, i.e. a retry storm aimed at an already-failing target.
  - **Every `[wal_tap]` numeric knob is range-checked at boot as well as by
    `PUT`,** through one shared `WalTapConfig::check_limits`. The config file
    was the way around the API's validation: `PUT
    {"min_retained_generations": 100}` is a `400` because the knob costs
    `n × storage.wal_max_size_mb` per WAL shard per index, while `xerj.toml`
    took the same `100` in silence and the node then held 100 rotated
    generations per shard forever. Bounds are `poll_interval_ms 50..=60000`,
    `max_retry_backoff_secs 1..=86400`, `min_retained_generations <= 64`, and
    `max_batch_docs` / `max_batch_bytes` / `request_timeout_secs` at least 1.
    Same precedent as `compression.block_size_docs` (#318): an out-of-range
    value is a typo the operator should hear about at boot, not as a disk-full
    page later.
  - XERJ's own `.xerj*` system indices are never shipped, whatever the allowlist
    says, and a wildcard never expands to a hidden index.
  - There is no backfill. A new index is shipped whole; an established one ships
    from the moment it is allowlisted. Seed the target with snapshot/restore
    first if it needs the existing documents.

### Changed

- **BREAKING — `PUT /{index}` now refuses an `analysis` block XERJ cannot
  honour, instead of accepting it and analysing differently**
  ([#204](https://github.com/xerj-org/xerj/issues/204)).
  `AnalyzerRegistry::apply_settings` is total by design (it also runs when an
  existing index is opened, where a hard failure would brick it), so an
  unresolvable tokenizer silently became `standard` and an unresolvable token
  filter was dropped — the index was created, `GET /_settings` echoed the block
  back, and the field was analysed in a way nobody asked for. Index creation
  now validates the block and answers `400` with the exact constructs it cannot
  build. `PUT /{index}/_settings` with an `analysis` block is likewise refused
  (it only ever changed what `GET /_settings` echoed; the registry is built
  once, at create/open, and there is no rebuild path — Elasticsearch also
  refuses analysis updates on an open index). That refusal covers every
  spelling this handler *accepts*, not just the nested one: the flat dotted
  `{"index.analysis.analyzer.x.type": "custom"}` and half-dotted
  `{"index": {"analysis.analyzer.x.type": "custom"}}` forms that ES clients
  routinely send used to walk straight past it into the display copy, and
  `GET /_settings` then echoed back an analyzer that analysed nothing.

  **This rejects settings blocks that XERJ used to accept with a `200`, and the
  ES-YAML conformance suite does not cover them — CI being green is not
  evidence that your index still creates.** What is honoured:

  | | honoured (still `200`) |
  |---|---|
  | `analysis.filter.*.type` | `synonym`, `length`, `shingle`, `asciifolding` |
  | `analysis.tokenizer.*.type` | `ngram`, `edge_ngram`, `pattern` |
  | `analysis.analyzer.*.type` (non-custom) | any name the built-in registry resolves — measured: `standard`, `english`, `whitespace`, `keyword` |
  | `analysis.char_filter` | nothing — the char-filter slot is hard-coded empty |
  | `analysis.normalizer` | nothing — never built |

  Anything else in those positions is now a `400`. There is one important
  exception to read first: XERJ resolves a declared filter or tokenizer by
  **name**, not by `type`, so a declaration whose *name* is a built-in is still
  honoured whatever its type says. Measured, all still `200`:
  `{"english_stop": {"type": "stop", "stopwords": "_english_"}}`,
  `{"lowercase": {"type": "lowercase"}}`,
  `{"stemmer"|"english_stemmer": {"type": "stemmer", "language": "english"}}`
  — the canonical Elasticsearch-docs `rebuilt_english` block among them, which
  has a regression test pinning it. Naming a built-in analyzer on a field
  (`"analyzer": "english"`) needs no `analysis` block at all and is unaffected.

  What now fails is a declaration under a name that does NOT resolve — which is
  the usual ES habit of naming filters after what they do in *your* index.
  Measured blocks that returned `200` in rc.14 and return `400` from this
  release:

  - a `keyword` field's `normalizer` (any);
  - a `char_filter` (any, including `html_strip` and `mapping`), declared
    either under `analysis.char_filter` or as an analyzer's `char_filter`;
  - a custom-named token filter whose `type` is `lowercase`, `stop`, `stemmer`,
    `snowball`, `kstem`, `porter_stem`, `ngram`, `edge_ngram`,
    `word_delimiter_graph`, `trim`, `unique`, `elision`, `decimal_digit`,
    `apostrophe`, `limit`, `truncate`, `pattern_replace`, `keyword_marker`,
    `uppercase` or `reverse` — e.g. `{"my_lowercase": {"type": "lowercase"}}`,
    which never lowercased anything, because the analyzer referencing
    `my_lowercase` found no such filter and dropped it;
  - a custom-named tokenizer whose `type` is `standard`, `whitespace`,
    `keyword`, `classic`, `uax_url_email`, `path_hierarchy`, `char_group`,
    `letter`, `lowercase` or `simple_pattern`;
  - an analyzer whose non-custom `type` is `simple`, `stop`, `pattern`,
    `fingerprint`, or a language other than `english`;
  - a `synonym` filter whose `synonyms` is a bare string rather than an array
    (it built a filter with zero rules).

  **The gate covers every spelling, at create as well as at update.** An
  `analysis` declaration written with flat dotted keys
  (`{"index.analysis.filter.my_lower.type": "lowercase"}`) or half-dotted ones
  (`{"index": {"analysis.analyzer.a.type": "custom"}}`) is the same request as
  the nested form to an Elasticsearch client, and this handler already parses
  dotted keys for other settings (`index.sort.field`). The analyzer registry
  resolves the nested spellings only, so a dotted declaration was accepted,
  echoed back by `GET /{index}/_settings`, and applied to nothing. It is now a
  `400` naming the offending key, on both `PUT /{index}` and
  `PUT /{index}/_settings`. Measured on this release, the same
  `my_lower`/`lowercase` declaration in four spellings — nested, `index.`-
  namespaced, fully dotted, half-dotted — answers `400`, `400`, `400`, `400`;
  a settings body with no analysis in it (`index.number_of_shards`,
  `index.sort.field`) still answers `200`. Nothing already on disk changes
  meaning: the dotted form is refused, not newly honoured, so no existing
  index's analysis is re-derived on reopen.

  **Upgrading:** there is no opt-out flag. Either remove the analysis block and
  name a built-in analyzer on the field (`"analyzer": "english"`), or keep only
  the constructs in the table. The `400` names every offending construct in one
  response, so one round-trip is enough to see the whole list. **Two paths are
  not covered and still accept these blocks silently: an index template's
  `template.settings.analysis` is dropped wholesale before the gate ever sees
  it, and option-level divergence inside a supported type (`synonyms_path`,
  `length.min`, `asciifolding.preserve_original`) is not checked at all.**

- **BREAKING — `PUT /_ingest/pipeline/{id}` refuses processor configuration
  XERJ cannot honour, so a pipeline that used to be acknowledged can now be a
  `400`, and a stored one can start refusing writes**
  ([#204](https://github.com/xerj-org/xerj/issues/204)). This is the same
  change as the "`200 acknowledged` … means we will run this" entry under
  *Fixed* below, stated from the upgrader's side, because it converts `201`
  into `400` for any pipeline using an ES processor or option this build does
  not implement. Read that entry for the full list.

  **What is NOT broken, deliberately.** A definition persisted by a build with
  **no gate** was accepted by whichever build wrote it, and there is no caller
  left to answer `400` at boot. Boot replay therefore compiles *those*
  definitions in a compatibility mode that reproduces the previously shipped
  behaviour for every check this sweep added (unknown `grok` pattern name,
  unknown `convert` target, a bare string where an array is required,
  `set.copy_from`, a processor-level `if`/`on_failure`/`ignore_failure`), so
  **an upgrade alone never turns a running cluster's writes from `201` into
  `400`**. Each one is logged at ERROR naming the pipeline and the offending
  config, and recorded in `Engine::degraded_pipelines`; a re-`PUT` of the same
  definition answers `400` with the same reason, which is the repair path. A
  definition that did not compile before the upgrade either (unknown stage
  type, missing required field) still fails at boot and is recorded unrunnable,
  as before.

  **Which definitions those are is decided by provenance, not by the calendar.**
  A definition THIS build refused at `PUT` time is recorded in
  `<data_dir>/ingest-pipeline-refusals.json` and replayed at *definition*
  strictness, so the refusal survives a restart. Without that marker the replay
  could not tell "written by an older, laxer build" from "written by this build
  and answered `400`", and a plain restart overturned the answer the operator
  had already been given: measured, `PUT` a `{"remove": {"field": "secret"}}`
  pipeline, add an `if` guard to it (refused — every write `400`s), restart the
  node and nothing else, and the identical write answered `201` with `secret`
  dropped from a document the guard EXCLUDES. It covered every check the sweep
  added; only an unimplemented *stage* survived a restart, because the raw ES
  body it stores does not deserialise. This is the same pattern as an index's
  `<index>/analysis-binding.json`: the marker's ABSENCE is the load-bearing
  signal.

  The marker is provenance, not a tombstone. A marked definition that compiles
  cleanly on the new build — because that build implements what the refusing
  one could not — clears its own marker at boot and runs, with an INFO line; a
  successful re-`PUT` or a `DELETE` clears it too. If the file exists but
  cannot be read or parsed, no pipeline's provenance is known, so **every**
  persisted pipeline is replayed at definition strictness and any that fails is
  recorded unrunnable with the file named in the reason — fail closed and
  loudly, rather than silently reopening the hole. **In that state a refusal is
  also not acknowledged:** the file is written whole from the in-memory map,
  which a failed load leaves empty, so the next refused `PUT` would have
  replaced it with a one-entry map and erased every refusal it still held —
  after which it parses cleanly, every earlier refusal reads as "no provenance",
  and the fail-closed behaviour converts itself into the hole it exists to
  close after exactly one process lifetime. `PUT /_ingest/pipeline/{id}` now
  answers `500` naming the file instead, and the file is left byte-for-byte as
  found; move it aside and restart to clear the state. An older binary reading a
  data dir written by this one simply ignores the file; it is a sidecar
  precisely because `cluster_state.json`'s format-1 envelope is
  `deny_unknown_fields` and widening it would break downgrades.

  **The marker survives concurrent refusals, not just sequential ones.** The
  sidecar is rewritten whole, so two refused `PUT`s in flight at once were a
  lost update rather than a merge: each snapshotted the map, and whichever
  wrote second with the older snapshot erased the other's marker — while both
  callers were told `{"acknowledged": true}`. Measured at a live node against
  the first cut of this work: 40 concurrent refused `PUT`s, 40 acknowledged, 37
  markers on disk, and after a plain restart the three that were lost answered
  `201` and dropped `secret` from a document their `if` guard excludes. The
  snapshot and the write now happen under the same `cluster_state_write` mutex
  that `flush_cluster_state` has always taken. Measured on this release: 40
  concurrent refused `PUT`s, 40 acknowledged, 40 markers on disk, plain restart,
  40 of 40 still `400`.

  The same rule applies one level up. Teaching the analysis resolver the
  canonical `settings.index.analysis` nesting (see the analysis entry above) is
  a fix at index *create* and a data bug at index *open*: `settings.json` keeps
  the caller's nesting verbatim, so an index created before this release with
  the canonical shape has postings that were written with `standard`, and
  honouring the declaration on reopen would tokenise every new write
  differently from everything already on disk. An index now records the
  analysis binding it was created with (`<index>/analysis-binding.json`) and
  keeps it for life. An index with no marker and analyzers declared *only*
  under `index.analysis` keeps the legacy binding if it holds documents — with
  an ERROR naming the index and saying a reindex is the repair — and adopts the
  canonical binding if it is empty, recording that so a later restart cannot
  disagree.

### Performance

- **A field mapped `dense_vector` — and the undeclared `_chunks` companion the
  passage pipeline writes beside it — no longer gets a full-text term
  dictionary** ([#328](https://github.com/xerj-org/xerj/issues/328)). Both
  halves are named in that headline because the companion is where most of the
  bytes are: of the 39,796,380 B removed below, **26,518,005 B (67%) is
  `<seg>.emb_chunks.fst`**, and in that corpus `emb_chunks` is not in the
  mapping at all — it is matched by NAME, not by anyone declaring it
  `dense_vector`. The field was fed
  through the lexical indexing path and given an FST + postings that no query
  path can read: kNN is served by `hnsw/graph.bin`, `exists` from
  `_source`/doc values, and `_field_caps` and highlighting never open the
  field's term dictionary. `extract_field_text`'s array arm joined the
  components with spaces, so a 128-dim vector became one enormous
  decimal-string term per document.

  Measured on a 5,000-doc × 128-dim corpus (text + keyword + long +
  `dense_vector` + its `_chunks` companion), after `force_merge(1)`, summing
  every file in the data directory except the write-ahead log:
  **54,068,549 B → 14,251,975 B (−73.6%, 3.79×)**. Per file, `<seg>.emb.fst`
  13,256,659 B → 0, `<seg>.emb.post` 10,834 B → 0, `<seg>.emb.norms` 48 B → 0,
  `<seg>.emb_chunks.fst` 26,518,005 B → 0, `<seg>.emb_chunks.post` 10,834 B → 0,
  while the three lexical `.fst` files (`body` 278 B, `cat` 53 B, `n` 209 B) are
  byte-identical on both sides.

  `.wal` is excluded from that total because its tail is reclaimed
  asynchronously and it is not reproducible: on the 300-doc fixture the
  committed regression test uses, runs put the *whole directory* anywhere
  between 858,147 B and 1,105,001 B while the durable bytes moved by ~500 B. The
  test prints both and asserts on the durable one. It reports
  **3,249,323 B → 857,265–857,769 B** for that fixture (before-side measured by
  reverting `index.rs` and `memtable.rs` to the merge base `ca4d75a` on the same
  fixture), which is the same −73.6% at 1/17th the corpus. Eight runs of the
  committed test on this revision printed 857,265 / 857,529 / 857,533 /
  857,545 / 857,549 / 857,657 / 857,657 / 857,769 B, a 504 B spread; the ceiling
  it asserts is 1,500,000 B, three orders of magnitude clear of that jitter and
  still 1.7 MB below the before-side.

  A `dense_vector` nested under an object mapping is covered too — its
  components were landing in the *parent's* term dictionary, because the segment
  builder flattens the whole object into one text field. On the nested fixture
  `<seg>.passages.fst` goes 398,558 B → 45 B, for both mapping shapes that reach
  `FieldType::Vector` (a dotted top-level `passages.vec`, and a `vec`
  sub-mapping under a `passages` object).

  The exclusion covers the whole family a vector field generates, not just the
  base name: `<field>_chunks` (the per-document multi-vector) and
  `__xerj_passage_meta__<field>` are excluded on the same walk that decides
  which fields get an HNSW graph. The companion is the bigger half —
  26,518,005 B against `emb.fst`'s 13,256,659 B, 49% of the whole pre-change
  index — so a base-name-only exclusion would leave more behind than it removes.

  **Two rules, and they do not have the same strength.** The base field is
  excluded BY DECLARED TYPE (`dense_vector`, unconditionally). The two
  companions are excluded BY NAME, because they arrive in `_source` with no
  mapping entry behind them, so there is no declared type to read — and a
  name-based rule YIELDS to any declaration it finds. **If you have mapped your
  own field called `<vector>_chunks` or `__xerj_passage_meta__<vector>` as
  anything at all other than `dense_vector` — `text`, `keyword`, `long`,
  `integer`, `short`, `byte`, `unsigned_long`, `double`, `float`, `half_float`,
  `scaled_float`, `date`, `date_nanos`, `boolean`, `ip`, `geo_point`, `binary`,
  `object`, `nested`, or a type string XERJ does not recognise — nothing about
  it changes**: it keeps its term dictionary, every query naming it keeps
  answering exactly what it answered before, and only the `dense_vector` beside
  it loses its postings. The same holds for a field of that name XERJ mapped for
  you dynamically, whatever type it inferred, as long as the value is not itself
  a multi-vector. Measured on
  `{text body, keyword emb_chunks, dense_vector emb}`, 50 docs, half `tenant-a`
  (before → after): `term {emb_chunks:"tenant-a"}` 25 → 25,
  `terms` 25 → 25, `bool{filter:[term emb_chunks]}` 25 → 25,
  `bool{must:[match body],filter:[term …]}` 25 → 25,
  `constant_score{filter:term}` 25 → 25,
  `bool{must_not:[term emb_chunks]}` 25 → 25, `<seg>.emb_chunks.fst`
  54 B → 54 B, `<seg>.emb.fst` 16,713 B → 0. Same with a `text` mapping:
  `match` / `match_phrase` / `multi_match` / `simple_query_string` / `prefix`
  all 25 → 25 and `<seg>.emb_chunks.fst` 71 B → 71 B. A `long` mapping keeps a
  44 B `.fst` and a `double` mapping a 48 B one, both 25 → 25 on `term`,
  `terms`, `range`, `bool{filter}` and `bool{must_not}`. A field of that name
  XERJ mapped for you *dynamically* is exempt on the same rule — measured
  25 → 25 for a `text` one (54 B `.fst`) and 25 → 25 for a `long` one
  (44 B `.fst`).

  **What makes "yields to any declaration" affordable** is that the companion
  is stopped from ever acquiring one. `<vector>_chunks` is user-supplied
  `_source` — an array of float arrays — so dynamic mapping used to register it
  like any other unmapped key and infer `double` from it, producing a mapping
  entry indistinguishable from one you wrote. Dynamic mapping now declines to
  register a `<vector>_chunks` key when a `dense_vector` named `<vector>` is
  already mapped *and* the value is a rectangular, non-empty array of non-empty
  all-numeric arrays — the exact multi-vector shape, and nothing else. A scalar,
  a string, a flat array or a ragged one under that name is your field and is
  mapped as one. Measured on `{text body, dense_vector emb}` with `emb_chunks`
  left out of the mapping, 50 docs: an `[[…],[…]]` value gets no mapping entry,
  `<seg>.emb_chunks.fst` = 0 B and a lexical clause on it answers 0; a `"tenant-a"`
  value is mapped `text`, keeps a 54 B `.fst` and answers 25; a `7` value is
  mapped `long`, keeps a 44 B `.fst` and answers 25.

  Both halves were measured on their own, by disabling each in the tree and
  re-running the committed tests:

  | | `emb_chunks.fst` (300-doc) | durable index | declared `long` sibling |
  |---|---:|---:|---|
  | this release | **0 B** | 857,265–857,769 B | 44 B `.fst`, answers 25 |
  | yield restored to `long`/`double`-excluded | 0 B | 857,445 B | **0 B, answers 0, `must_not` returns 50** |
  | dynamic-mapping refusal disabled | **1,592,118 B** | **2,451,336 B** | 44 B, answers 25 |

  The middle row is the behaviour rc.16 would have shipped; the bottom row is
  the 67% of the saving that an unconditional "any declaration wins" gives up.
  Neither cost is paid here.

  A lexical clause that NAMES a `dense_vector` (or one of its companions) is now
  lowered to `match_none` at plan time rather than falling through to the
  stored-doc scan. That is the correctness half of the change, not only the
  speed half: with the postings removed and no lowering,
  `{"multi_match":{"fields":["emb"],"query":"0"}}` returns **every document**
  instead of none, because the scan renders the float array to text and every
  component contains a `0`. Measured on the corpus above, first call for each
  shape (`main` → postings-gone-without-lowering → this release):

  | query | `main` | no lowering | this release |
  |---|---|---|---|
  | `multi_match {fields:["emb"], query:"0"}` | 0 hits, 0.409 ms | **5,000 hits**, 123.5 ms | 0 hits, 0.012 ms |
  | `multi_match {fields:["emb_chunks"], query:"0"}` | 0 hits, 2.24 ms | **5,000 hits**, 138.5 ms | 0 hits, 0.014 ms |
  | `term {emb: "<component>"}` | 0 hits, 89.2 ms | 0 hits, 186.8 ms | 0 hits, 0.061 ms |
  | `range {emb: {gte:-2, lte:2}}` | 0 hits, 119.8 ms | 0 hits, 134.5 ms | 0 hits, 0.009 ms |
  | `simple_query_string {fields:["emb"]}` | 0 hits, 0.033 ms | 0 hits, 114.5 ms | 0 hits, 0.015 ms |
  | `constant_score {filter: term emb}` | 0 hits, 83.0 ms | 0 hits, 110.3 ms | 0 hits, 0.012 ms |
  | `exists {field: "emb"}` | 5,000 hits, 83.0 ms | 5,000 hits, 108.0 ms | 5,000 hits, 88.5 ms |
  | `match {body: "liquidity"}` | 5,000 hits, 5.18 ms | 5,000 hits, 5.96 ms | 5,000 hits, 4.95 ms |
  | `term {cat: "even"}` | 2,500 hits, 2.32 ms | 2,500 hits, 2.87 ms | 2,500 hits, 2.27 ms |

  `exists`, `knn` and `semantic` on the field are untouched, a mixed `fields`
  list keeps its lexical members, and a `nested` query is deliberately not
  rewritten (its field names resolve against the element, not the root).

  **Four behaviour changes to know about:**

  - **A `dense_vector` (and its undeclared `_chunks` companion) now has no
    lexical surface at all: every lexical leaf naming one *outright* answers 0
    hits.** "Outright" excludes one shape, measured rather than assumed: a
    `fields` entry that is a PATTERN (`["emb.*"]`) is NOT lowered, because
    resolving a pattern needs the expansion universe — dynamic fields included —
    and lowering one that would have expanded onto a real lexical field is the
    silent-zero failure this release goes out of its way to avoid. So
    `{"multi_match":{"query":"0","fields":["emb.*"]}}` still answers 300 of 300
    on the 300-doc fixture, exactly as on `main`: the unprojectable clause falls
    to the stored-doc scan, which resolves the pattern against the source's
    field names and matches the rendered decimals. Unchanged in both columns, so
    not a regression — but not closed either. `["emb"]`, `["emb*"]` and `["*"]`
    all answer 0. Before this release
    such a clause fell through to the stored-doc scan, which renders the float
    array back to decimal text and matches against *that*, so whether a shape
    answered 0 or answered most of the corpus depended entirely on whether the
    rendered decimals happened to contain the probe. This is a CLASS of changed
    answers, not one row. Measured on the 300-doc × 128-dim regression fixture,
    `main` → this release:

    | query | before | after |
    |---|---:|---:|
    | `wildcard {emb: "0*"}` | **300** (every document) | 0 |
    | `fuzzy {emb: {value:"0", fuzziness:2}}` | **300** (every document) | 0 |
    | `wildcard {emb_chunks: "0*"}` | **300** (every document) | 0 |
    | `fuzzy {emb_chunks: {value:"0", fuzziness:2}}` | **300** | 0 |
    | `prefix {emb: "0"}` | 146 | 0 |
    | `prefix {emb_chunks: "0"}` | 146 | 0 |
    | `match_phrase_prefix {emb: "0"}` | 50 | 0 |
    | `term {emb: <exact component>}` numeric | 1 | 0 |
    | `terms {emb: [<exact component>]}` numeric | 1 | 0 |

    Shapes whose probe text never appeared in a rendered float (`term` with a
    string, `match`, `match_phrase`, `regexp`, `range`, `simple_query_string`,
    `query_string`) answered 0 on both sides. ES rejects these queries on a
    `dense_vector` outright and Lucene gives a field with no terms an empty
    scorer, so 0 is the answer this release adopts for all of them. The
    regression test pins the whole class rather than sampling it.
  - **A `bool` that mentions a `dense_vector` can change `_score` and
    `max_score` without changing its hit count.** Dropping the dead clause makes
    the surviving bool projectable onto the inverted index, so BM25 scores it
    where the stored-doc scan used to. Measured on the same fixture:

    | query | hits | `_score` | `max_score` |
    |---|---|---|---|
    | `bool{should:[term emb, term cat:"even"]}` | 150 → 150 | 0.008402659 → 0.6931471 | 1.6931472 → 0.6931471 |
    | `bool{must:[match body], must_not:[term emb]}` | 300 → 300 | 0.008402659 (unchanged) | 1.6931472 → 0.008402659 |

    Both moves are toward consistency — on `main` these queries reported a
    `max_score` that **no returned hit carried**, and the first now scores
    exactly like `{"term":{"cat":"even"}}` on its own, which is what it reduces
    to. Absolute scores are not part of XERJ's compatibility surface, but
    anything comparing scores across a version boundary or asserting on
    `max_score` for a bool that touches a vector field will see this. Plain
    lexical queries are unaffected; the blast radius is bools with a
    `dense_vector` clause in them.
  - **`GET /<index>/_mapping` no longer lists a `<vector>_chunks` field that
    XERJ mapped for you.** The multi-vector companion beside a declared
    `dense_vector` used to be registered by dynamic mapping as `double` (an
    array of float arrays walks to its first scalar). It is not registered at
    all now, so it disappears from `_mapping` and `_field_caps` — the same
    treatment `__xerj_passage_meta__<vector>` already had. Nothing that reads
    `_source` changes; the values are stored and returned exactly as before, and
    `knn` and `semantic` on the vector itself are untouched. A field of that
    name that you mapped yourself, or that XERJ mapped from a non-multi-vector
    value, is unaffected and still listed.

    Aggregating and sorting on that companion still answer `200` — measured on
    the wire against a live node with `emb` mapped `dense_vector` and
    `emb_chunks` unregistered: `terms` returns real buckets, `stats` returns
    `count: 0` byte-identical to the previous release, and `sort` returns sort
    values. Nothing that worked before returns an error now.
  - **Existing indices keep their bloat until a merge rewrites them, and an
    index created by rc.16 or earlier only ever reclaims the base-vector half.**
    This is a write-side rule. Segments written by an earlier release keep their
    `.emb.*` / `.emb_chunks.*` files, and the per-segment `fts_has_field` gate
    reads whichever shape it finds.

    `POST /<index>/_forcemerge?max_num_segments=1` reclaims `.emb.*`. **It does
    not reclaim the companion.** Measured by building an index on an rc.16
    server (`emb` mapped `dense_vector` before ingest, 500 docs × 32 dim with
    `emb_chunks` multi-vectors), then reopening the same data directory with
    this release and force-merging: `.emb.fst` 262,904 B → 0, but
    `.emb_chunks.fst` 525,200 B → 525,355 B, and
    `wildcard {emb_chunks: "0*"}` still answers 501 of 501. The reason is not a
    bug in the merge: rc.16 persisted `emb_chunks: {"type": "double"}` into the
    mapping, and the by-name rule correctly yields to a field you (or an earlier
    release) declared. Since a mapping entry cannot be dropped in place, the
    remediation for an existing index is to **reindex into a fresh index**. On
    the corpus this entry headlines that companion is 67% of the saving, so an
    in-place upgrade recovers roughly a third of the published number.

    The companion rule has an ordering limit of the same kind, and it fails in
    the safe direction: XERJ can only decline to map a companion it can
    recognise, and recognising one needs the `dense_vector` to be mapped
    *before* the documents arrive. Index first and add the mapping afterwards
    and `<vector>_chunks` already carries a dynamic `double` entry, which the
    by-name rule then yields to — so that index keeps the companion's term
    dictionary (measured 9,467 B on a 50-doc × 16-dim fixture) instead of
    reclaiming it. It costs bytes, not answers.

  *Known limitation, filed as [#382](https://github.com/xerj-org/xerj/issues/382):*
  refusing a `FieldConfig` for the companion leaves the pre-existing schema-hash
  throttle asserting the key set is unchanged, so if a later document puts a
  differently-typed value under `<vector>_chunks` it stays unregistered for up
  to 100 documents and queries on it answer zero until the throttle expires.
  Doc-order-dependent and self-healing. The throttle is pre-existing and the fix
  belongs to it.

  *Not changed:* ES rejects `term`/`match`/`range` on a `dense_vector` outright,
  where XERJ answers `200`. This release removes bytes and adopts Lucene's
  zero-hit answer; it does not add the rejection.

### Fixed

- **`200 acknowledged` on an ingest pipeline now means "we will run this", and
  every write path enforces it**
  ([#204](https://github.com/xerj-org/xerj/issues/204)).
  `PUT /_ingest/pipeline/{id}` used to store the raw body first and only
  warn-log a compile failure, so a pipeline that could never run was
  acknowledged and echoed back by `GET`. It now compiles first, with three
  outcomes: compiled → stored and acknowledged; a processor config we refuse →
  `400`; a processor or option this build cannot honour → accepted (as
  Elasticsearch does) but recorded as **unrunnable**, so the write refuses
  rather than storing an untransformed document.

  Defects this surfaced, each of which answered `200`/`201` and did something
  smaller than asked: **every** ES `rename` processor failed to compile (the
  handler emitted `{from, to}`, the plugin requires `{mappings: {…}}`) and
  renamed nothing; ES `append` was executed as `set`, replacing the field
  instead of extending it, and disagreeing with `_simulate`, which implemented
  a real append; an ES `set` processor did not override an existing value where
  Elasticsearch does (the ES→XERJ translation now writes `"override": true`
  explicitly when the processor omits it — the **native** `type: "set"` stage
  keeps its long-standing `override: false` default, because native definitions
  are recompiled from `cluster_state.json` at every boot and flipping the stage
  default would have changed what an already-running cluster's pipelines do to
  documents on a binary upgrade alone); an unknown `pii_redaction` type built an
  empty pattern set and redacted nothing; an unknown `grok` pattern name
  silently became the
  catch-all; an unknown `convert` target left the field unconverted per
  document; array-typed keys given as a bare string were dropped and the stage
  behaved as if unconfigured; and `DELETE /_ingest/pipeline/{id}` removed the
  definition while leaving the compiled pipeline live under the same name.
  ES `convert` `long`/`double` now map to XERJ's equivalent `integer`/`float`
  rather than being refused.

  Two more of the same, at the definition's own edges: a **pipeline-level**
  `on_failure` block (the top-level body key, not the processor-level one) was
  accepted, echoed by `GET`, and never run — the pipeline is now recorded
  unrunnable, exactly as its processor-level twin already was; and `set` with
  Elasticsearch's `copy_from` is reported as an unsupported *option* (accepted
  at `PUT`, refused at the write) rather than as `mapper_parsing_exception:
  missing value`, which told the caller their valid ES pipeline was malformed.

  **The refusal reads the value, not the key.** A processor-level option is
  refused only when what the caller *wrote* asks for behaviour this build does
  not have; writing the value XERJ already implements is the same as omitting
  it. `ignore_failure: false` is Elasticsearch's own default and ES clients emit
  it verbatim — a presence check made that pipeline register and then `400`
  every write through it, while the byte-identical processor without the key
  indexed fine. Accepted: `ignore_failure: false`, an empty `on_failure: []`,
  `ignore_missing: true` (which is exactly what `remove`/`rename`/`convert`/
  `date` already do — they pass the document through unchanged when the field
  is absent), `ignore_empty_value: false`, `allow_duplicates: true`. Refused:
  the opposite value in each case, and any `if` at all. (Modelled on Lucene's
  consume-then-complain: `AbstractAnalysisFactory.getBoolean(args, name,
  default)` at `lucene/core/src/java/org/apache/lucene/analysis/AbstractAnalysisFactory.java:213-217`
  removes the key and returns the default only when it was absent, and
  `ArabicStemFilterFactory` at
  `lucene/analysis/common/src/java/org/apache/lucene/analysis/ar/ArabicStemFilterFactory.java:44-51`
  then throws on whatever is left. Lucene is Apache-2.0, the same licence as
  XERJ; no code is reproduced.)

  **`remove` and `rename` no longer walk around the gate.** Their ES→XERJ
  translation built a fresh config object holding only the translated keys, so
  a processor-level `if` / `on_failure` / `ignore_failure` / `ignore_missing` on
  them was dropped *before* the gate ran: `{"remove": {"field": "secret", "if":
  "ctx.tenant == 'a'"}}` compiled, was acknowledged, and dropped the field on
  every document — while the identical `if` on `set` was refused. Every branch
  of the translation now edits the caller's config instead of rebuilding it.

  **`_simulate` speaks Elasticsearch's vocabulary again.** It walks the
  *stored* pipeline, and once ES `rename` started compiling the stored form
  became XERJ's internal `stages` shape — whose stage names leaked straight into
  the response, so `processor_type` read `field_rename` where ES reports
  `rename`. The stages→processors conversion now inverts both halves of the
  translation, name and config.

- **`GET /_ingest/pipeline[/{id}]` answers in Elasticsearch's `processors`
  vocabulary instead of XERJ's internal `stages` shape**
  ([#204](https://github.com/xerj-org/xerj/issues/204)). Two shapes reach the
  store: the translated `stages` body (everything that compiles) and the raw ES
  body (kept verbatim for a definition XERJ stores but cannot run). Only the
  second was ever ES-shaped, so `GET` spoke XERJ's vocabulary for every pipeline
  that *worked* — measured before this release, an ES `rename` pipeline came
  back as
  `{"p":{"description":"d","stages":[{"type":"field_rename","config":{"mappings":{"a":"b"}}}]}}`.
  This is a wire-visible change on both `GET /_ingest/pipeline/{id}` and
  `GET /_ingest/pipeline`. It also mattered beyond cosmetics: `GET`'s output is
  what an operator edits and `PUT`s back, and a `stages`-shaped body takes the
  handler's pass-through branch — which is exactly how the restart hole
  described under *Changed* was reached in ordinary use.

  `set` is rendered with an explicit `"override"`, always, because it is the one
  key whose default differs between the two vocabularies (Elasticsearch `true`,
  XERJ's native `set` stage `false`). Emitting a bare ES `set` for a native
  stage would claim it overwrites when it preserves. A pipeline created through
  `PUT /_ingest/pipeline` therefore shows `"override": true` even when the
  caller omitted it — that is the value XERJ applies, and it round-trips: PUTting
  `GET`'s own output back cannot silently flip the stage's behaviour. Relatedly,
  `_simulate`'s `set` interpreter honoured no `override` at all and always
  overwrote, so it reported a document the ingest path would never produce; it
  now applies Elasticsearch's rule (default `true`; `false` leaves an existing
  non-null value alone).

  **This is also the only read endpoint for a pipeline created through
  `PUT /v1/pipelines/{name}`** — the native surface registers no `GET` — so the
  round trip has to be lossless in xerj's own vocabulary too, and it was not.
  `PUT /_ingest/pipeline/{id}` rebuilt the stored definition as
  `{description, stages}` and dropped every other top-level key, so PUTting
  `GET`'s own output back silently discarded a natively-defined pipeline's
  `on_error` and `timeout_ms`: the error policy reverted from `pass` (keep the
  document) to the `Drop` default (discard it) under a
  `200 {"acknowledged": true}`. The translation now edits the body instead of
  rebuilding it — the same rule already applied to `remove`/`rename` one level
  down. Measured on this release, `{"description":"n3","on_error":"pass",
  "timeout_ms":250,"stages":[…]}` reads back with both keys intact, and PUTting
  that body back leaves them intact, before and after a restart. Elasticsearch's
  own pipeline metadata (`version`, `_meta`, `deprecated`) rides along the same
  way and is now echoed back rather than dropped. `PUT /v1/pipelines/{name}`
  given that ES-shaped body used to answer `500` with `internal error: missing
  field stages`; it now answers `400` naming `/_ingest/pipeline/{name}` as the
  endpoint that accepts it.

- **Four wire-visible refusals that were fixed in this sweep but not written
  down** ([#204](https://github.com/xerj-org/xerj/issues/204)). All four
  replace a silent fallback with an error, which is the point of the issue, but
  each changes a status code:
  `POST /v1/admin/backup` answers `400` on a request body that is present but
  unparseable, where it previously ran a *default* backup under a success
  response; `GET /{index}/_stats` and `GET /_all/_stats` carry an added
  non-Elasticsearch key `primaries.mappings.schema_persist_failures` inside an
  otherwise ES-shaped response; the authorization layer's response pruning
  answers `500` when it cannot parse the body it was asked to filter, instead
  of falling back to the unfiltered bytes; and the bundled console's
  `DELETE /_xerj-console/api/v1/auth/passkeys/{id}` propagates a failed delete
  instead of discarding it — it answered `204 No Content` unconditionally, so a
  revoke refused by the engine (index write block, disk flood stage, storage
  error) reported success while the passkey stayed on disk and kept
  authenticating. An already-absent credential is still a `204`; only a genuine
  failure now surfaces.

  **The refusal now covers every ingest path, not only `PUT /{index}/_doc`.**
  `POST /_bulk?pipeline=`, `index.default_pipeline` under `_bulk`,
  `_reindex` `dest.pipeline` and `POST /{index}/_update_by_query?pipeline=`
  previously read the instruction and dropped it — a redaction pipeline that
  the single-document endpoint refused was ignored in bulk, and the document
  was stored verbatim under `errors: false`. All four now run the pipeline, and
  refuse when it cannot run. `_reindex` and `_update_by_query` validate the
  name before touching a single document, so a bad pipeline fails the request
  instead of half of it.

  Two related honesty fixes on the same paths: `_clone` / `_shrink` / `_split`
  copied at most 10,000 documents and swallowed every per-document write error
  before answering `{"acknowledged": true}` (now full keyset paging, and any
  failed write — or a failed source flush — fails the request); and a failed
  `_bulk` item no longer reports `"result": "deleted"` alongside its own
  `status: 400`, which claimed a deletion that never happened.

  **`ignore_missing: false` is refused on purpose, and it is not the same call
  as `ignore_failure: false`.** Both are Elasticsearch defaults that ES clients
  emit verbatim, so the difference is worth stating plainly: the rule is *accept
  the value this build actually implements*, and the two land on opposite sides
  of it. `ignore_failure: false` means "a failing processor is not ignored",
  which is exactly what XERJ does, so it is accepted. `ignore_missing: false`
  means "FAIL the document when the field is absent", which XERJ cannot do at
  all — `ProcessAction` has no error variant, and every field-reading stage
  passes the document through untouched. Accepting it would be precisely the
  silent lie this issue exists to remove, so it is refused: a `remove` /
  `rename` / `convert` / `date` processor that spells out `ignore_missing:
  false` registers and then `400`s every write through it. **Omitting the key
  is accepted** and behaves as `true`. That asymmetry — omission accepted,
  the ES default written out refused — is deliberate but is a hard `400` for
  ES-generated pipelines; strip the key, or set it to `true`, to state what
  XERJ will actually do. `ignore_empty_value` is the mirror image: omitting it
  or writing `false` is honoured exactly, `true` is refused.

  **Known gaps, stated rather than closed:** `_simulate` interprets the stored
  ES body directly and covers processors the compiled path does not, so a
  pipeline recorded as unrunnable can still show a transformation under
  `_simulate` that ingest refuses to perform; `GET /_ingest/pipeline/{id}`
  carries no runnability signal — it renders the stored definition in ES
  vocabulary whether or not this build can run it, so an operator only learns
  at write time; a write addressed to an **alias** does not pick up the
  backing index's `default_pipeline`; `_update_by_query` reports a `drop`
  processor as a per-document failure, because it updates in place and has no
  delete path; `Engine::degraded_pipelines` is in-memory only and has no
  HTTP surface yet, so a boot-replay degradation is visible in the startup log
  and nowhere else — unlike a definition-time refusal, which is now durable
  (see *Changed*); and a refusal marker is only read back at boot, so a data
  dir opened by two processes at once is outside what the sidecar can promise
  (the node lock already prevents that).

- **Unity assets were sampled through a 4 MiB cap, silently junking whole
  object classes.** Unity YAML is a grouped family — each `unity_class` is its
  own dataset and a class's first document can sit anywhere in a scene that
  `extract/unity.rs` itself says can exceed 200 MB. A class first appearing
  past the cap was never sampled, got no entry in the plan, and phase B then
  had nowhere to route its records: they became `file_junk` with nothing said.
  The cap is now 512 MiB for this family, **and** any record whose group was
  never sampled is now reported by name and count instead of silently counted
  as junk.

- **A `script_guid` that failed to resolve produced no counter, no warning and
  no report line.** `build_unity_guid_map` discarded `extract_meta`'s `Result`,
  so an unreadable `.meta` was indistinguishable from a script nothing
  references — on the feature's own headline query. Unreadable sidecars,
  sidecars carrying no usable guid, and unresolvable `script_guid`s are now
  each reported.

- **`script_path`/`script_class` were mapped only when the phase-A sample
  happened to contain a `script_guid`**, while phase B stamps them whenever a
  guid resolves. A cluster whose sampled window held no `m_Script` therefore
  got them dynamic-mapped at index time, feeding the field-budget overshoot in
  #312. They are now registered for every Unity cluster, and the enrichment
  runs *before* `coerce_record` rather than after it, so the fields it stamps
  are validated like every other field instead of bypassing coercion.

- **`build_unity_guid_map` ran serially, unmetered, on the critical path of
  every run** — including a resumed no-op incremental, which otherwise has no
  work to do. A real Unity project has 10k-500k `.meta` sidecars. It now runs
  on `crate::pool` under the progress meter (the unattributed-stretch pattern
  of #241).

- **A `--stub` file was named from the content path**, which under durable
  preparation is a content-addressed blob — so every stub's one and only field
  would have been titled after a blob ordinal (the #294 failure class).
  It now uses `Sniffed::logical_name`.

- **`order::band_from_family_str` disagreed with `order::band`.** The string
  form's catch-all ranked `binary` as `Bulk` where the enum form ranks it
  `Vendored`, so a resumed run could order work differently from the run that
  planned it. Both now agree for every family, pinned by a test that iterates
  the whole enum.

- **The autoindex use-case README documented three flags and behaviours that
  do not exist.** `--no-default-excludes` and `--no-gitignore` are spelled
  `--no-default-ignores` and `--no-ignore`, and `cli::parse` hard-errors on an
  unknown argument — so a reader who copied the documented invocation got an
  error instead of an index. The marker-gated pruning of `node_modules/` and
  `target/` was described but had been dropped from the branch. A test now
  parses every `--flag` named in that README and fails if the CLI would reject
  it.

- **`stub_matcher_tests::an_invalid_pattern_fails_loudly_at_startup` did not
  test its own name** — it asserted only that a *valid* pattern compiles.
  `glob_to_regex` escapes every metacharacter, so no glob can produce a
  syntactically invalid regex; the one reachable failure is the compiled-size
  limit, which `?` and `**` reach at ~10^5 characters. Over-long patterns are
  now rejected explicitly with a message naming the flag and the limit, and the
  test exercises that path.

### Changed

- **Two proposed byte-statistics binary heuristics were dropped before they
  shipped; raw TGA is detected by its header instead.** Both were attempts to
  recognise a raw texture that decodes into printable characters, and both
  turned out to be tests for "not written in Latin script".

  The first, from PR #274, classified any text over 4 KiB with under 5%
  whitespace as binary. `nonblank` is built from `text.lines()`, so newlines
  are already stripped and only intra-line whitespace counts — which means
  Chinese, Japanese, Korean, Thai, Lao, Khmer and Burmese prose, base64 blobs,
  FASTA sequences and minified single-line files all score 0%. This repo's own
  `failure_resume_http_tests::legacy_key_collision_fails_before_visibility_with_scoped_guidance`
  builds a 65,537-byte fixture of one repeated ASCII letter; with that guard
  reinstated the test fails with `left: 3, right: 0` — the run exits 3 instead
  of 0.

  The second was introduced by the first attempt to reland #274 and is
  **removed here**: "decoded via lossy windows-1252 AND over 30% non-ASCII is
  pixel soup". windows-1252 is the fallback every legacy 8-bit codepage decodes
  through, so it is not a test for image data. Measured through `sniff()` on
  `ca4d75a` versus that branch, with identical fixtures:

  | fixture | `ca4d75a` | with the guard |
  |---|---|---|
  | windows-1251 Russian, 13,000 B | `txt-prose` | `binary` |
  | KOI8-R Russian, 13,000 B | `txt-prose` | `binary` |
  | windows-1253 Greek, 12,200 B | `txt-prose` | `binary` |
  | windows-1255 Hebrew, 11,000 B | `txt-prose` | `binary` |
  | windows-1256 Arabic, 10,800 B | `txt-prose` | `binary` |
  | Shift-JIS Japanese, 22,401 B (byte pad 1; likewise pad 3) | `txt-prose` | `binary` |
  | Shift-JIS Japanese + ASCII code, 13,360 B | `txt-lines` | `binary` |
  | Shift-JIS Japanese + ASCII code, 66,800 B | `txt-lines` | `binary` |

  `scan_file` turns `Family::Binary` into `junk: binary content (unknown)`, so
  each of those files stopped being indexed. A `looks_like_legacy_cjk` escape
  hatch shipped with it and could not carry the weight: single-byte codepages
  never form valid Shift-JIS/GBK/Big5/EUC-KR double-byte pairs, so Cyrillic,
  Greek, Hebrew and Arabic were never rescued at all; it required a *lossless*
  trial decode while `sniff()` sees only the first 8192 bytes, so the same
  Japanese document was text at byte-offset pads 0 and 2 and binary at pads 1
  and 3; and a realistic Japanese technical document (prose around ASCII code
  fences) sits below its 30% ideograph floor while sitting above the guard's
  30% non-ASCII ceiling. Every fixture in the table above now classifies
  exactly as it does on `ca4d75a`.

  What replaces it is `looks_like_tga_header`: TGA has no magic number, but its
  18-byte header is constrained enough that bytes 1 and 2 of the file must both
  be control characters. **Headerless raw payloads (`.raw`, `.bytes`,
  uncompressed PCM) still classify as text**, exactly as they do on `ca4d75a`.
  Bounding what a magic-less binary costs is the fix for that and is not
  attempted here — see [#381](https://github.com/xerj-org/xerj/issues/381);
  `for_each_section` already streams, so the unbounded quantity is the record
  count, not resident memory.

- **The new magic-byte signatures now require structural confirmation.** Nine
  signatures (PSD, both TIFF byte orders, RIFF, OGG, FLAC, MP3, FBX and EXR)
  were added by the first reland attempt as bare `starts_with` tests. **Six** of
  them are entirely printable ASCII — `8BPS`, `RIFF`, `OggS`, `fLaC`, `ID3` and
  the 18 letters `Kaydara FBX Binary` — so they matched ordinary text. Measured
  through the real `sniff()` on `ca4d75a` vs this branch with identical
  fixtures:

  | fixture | `ca4d75a` | first reland |
  | --- | --- | --- |
  | CSV whose first column header is `ID3` | `csv` | `binary`/`mp3` |
  | prose opening `RIFF is a container format used by WAV files. …` | `txt-prose` | `binary`/`riff` |
  | prose opening `OggS pages carry the packets of an Ogg stream …` | `txt-prose` | `binary`/`ogg` |
  | prose opening `fLaC is the four byte magic of a FLAC audio …` | `txt-prose` | `binary`/`flac` |
  | prose opening `8BPS is the magic of an Adobe Photoshop …` | `txt-prose` | `binary`/`psd` |
  | `.md` note opening `Kaydara FBX Binary is the 20-byte magic …` | `txt-prose` | `binary`/`fbx` |

  Each of the six is now qualified by what must follow it: PSD version, RIFF
  FORM type, Ogg stream-structure version, FLAC block type, ID3v2 major version
  and synchsafe size, and for FBX the full 23-byte header (`Kaydara FBX
  Binary`, two spaces, NUL, `0x1A`, `0x00`). The FBX case was missed by the
  first pass at this fix and caught in review — at 18 printable characters it is
  the likeliest of the six to open a real sentence, and likeliest precisely
  inside the Unity/3D-asset corpus this feature targets. Every fixture in the
  table above now classifies exactly as it does on `ca4d75a`. The true positives
  are unchanged and still covered; the FBX true-positive fixture was corrected
  to the real 23-byte header, which is what any Autodesk tool emits.

  This is what Lucene's `CodecUtil.checkHeader` does with `CODEC_MAGIC`
  (`lucene/core/src/java/org/apache/lucene/codecs/CodecUtil.java:183`, which
  hands straight to `checkHeaderNoMagic` at `:202` and refuses the file unless
  the codec name *and* a version in range follow — the magic alone is never
  taken as proof). Apache-2.0, same licence as XERJ; adapted, not copied.

  **Not fixed here, deliberately:** `GIF8` and `BM` are printable-ASCII
  signatures with exactly the same defect, but they are **pre-existing** — they
  are unchanged from `ca4d75a`, and prose opening `GIF8`/`GIF89a`, a CSV whose
  first column header is `GIF8`, and prose opening `BM` were each measured to
  classify as `binary`/`gif` and `binary`/`bmp` on `ca4d75a` **and** on this
  branch, identically. Fixing them changes behaviour unrelated to Unity, so it
  is filed as **#380** rather than smuggled in here, and today's wrong answers
  are pinned by `gif8_and_bm_are_still_taken_on_faith` so #380 has to flip that
  assertion deliberately. `GIF8` is listed alongside `BM` so the next reader
  does not conclude `BM` is the only one.

- **A junk entry could overwrite a successfully indexed file's catalog row.**
  When phase B met a record group phase A never sampled, the worker pushed a
  whole-file junk entry while leaving `send_err` unset — so the file also
  reached `journal.file_done`, and both passes wrote `catalog::file_doc` under
  the same `file:{file_key}` id into the same bulk. The junk document landed
  second and won: a file that indexed records was reported in the catalog as
  status `junk` with `records: 0`, `files_junk` counted it, and
  `CodeCoverage::observe` ran for it twice. Reachable with no Unity involved,
  via a >64 MiB SQL dump whose first row for some table starts past
  `SQLDUMP_SAMPLE_LIMIT`. The unsampled group is now reported on the progress
  meter instead, the dropped records stay on that file's own completion where
  they already were, and the disjointness the code claimed in a comment is now
  enforced at the one place that holds both sets (`shadowed_junk_entries`).

### Known issues

- `ref_guids` (Unity) and `joints` (BVH) are multi-valued keyword arrays, and
  #332 means array elements are joined into one FTS token, so a post-flush
  `term`/`match` on a single element does not hit. `script_guid`,
  `script_path` and `script_class` are single-valued and unaffected, so the
  documented "which scenes use this script?" query is not impacted. Fixing
  #332 is an engine-side change to the FTS writer input type.

- **`GIF8` and `BM` are still unqualified printable-ASCII magic signatures**, so
  a text file whose first characters are `GIF8`, `GIF89a` or `BM` is classified
  `binary` and junked. This is **pre-existing, not introduced here** — measured
  identical on `ca4d75a` and on this branch (`binary`/`gif` and `binary`/`bmp`
  in both) — and is left unchanged because fixing it is a behaviour change with
  no Unity content. Both names are recorded here on purpose: `BM` is the
  obvious one and `GIF8` is the one a reader would otherwise miss. Tracked as
  **#380**; `gif8_and_bm_are_still_taken_on_faith` pins the current behaviour.

- **A magic-less binary is still sectioned and indexed in full.** The two
  byte-statistics guards this branch removed (they junked non-Latin text) also
  caught NUL-free binary payloads that decode printable under the
  windows-1252 fallback. Nothing replaced them except `looks_like_tga_header`,
  which covers TGA only. Measured on this branch: a 4,194,495-byte printable
  NUL-free blob named `texture.bytes` sniffs `txt-prose` and expands into 2048
  indexed records. This is **not** the peak-RSS bug from #239 —
  `for_each_section` is still streaming and per-file memory is still bounded;
  the unbounded quantity is the record COUNT. Removing the guards was still
  right: they deleted CJK, Cyrillic, Greek, Hebrew and Arabic documents
  worldwide. Tracked as **#381**.

- **`autoindex` aborted permanently on a corpus containing a byte-identical
  duplicate** (#345). A duplicate path is the only thing that makes a run issue
  the catalog's duplicate-alias `_delete_by_query` at all, and that call was
  fatal. Because it runs *after* every document is durably indexed and after the
  per-file journal has committed, a 5xx there turned a fully successful run into
  `xerj-done ok=false exit=1 reason=aborted` — and it stayed that way: the
  journal was complete, so every rerun indexed zero files and aborted on the
  same line. The sweep is metadata-only bookkeeping and is now reported instead
  of fatal: the run finishes `ok=true exit=3
  reason=catalog-alias-sweep-failed`, names the paths whose alias document was
  not swept, and records `catalog_alias_sweep_failed_paths` /
  `catalog_alias_sweep_error` on the run document. Exit `3` already means
  "completed, and something is recorded rather than fatal", so no new exit code
  was introduced — read `reason` to tell it apart from `completed-with-junk`.
  The catalog *bulk* that follows the sweep is still fatal, so a catalog index
  that cannot be written still fails the run.

- **Any 429/5xx from the server was reported as a bare status line.** The retry
  wrapper kept `HTTP 500 Internal Server Error` and discarded the response body,
  which is the same sentence for a poisoned index, a full disk and a panicking
  handler — the reason #345 was filed as "not investigated". A bounded prefix of
  the server's own `error.type: error.reason` (or the raw body, for a proxy's
  error page) now travels with the status on every `with_retry` call site.

- **The duplicate-alias sweep issued one HTTP request per duplicate path.** It
  now names up to 1024 paths per request with a `terms` filter, so a corpus with
  K duplicates costs `ceil(K/1024)` round trips instead of K — and has
  proportionally less surface on which to fail.

## [1.0.0-rc.16] - 2026-08-13

Both rc.15 known issues (the progress-stream forgery and the outer-`.gitignore`
reach into nested checkouts) are fixed in this release — see Fixed below.

### Performance

- **An idle node with many indices burned ~4 CPU cores and made the host
  machine unusable.** `IndexStore` spawned one OS thread per index that woke
  every `wal_batch_ms` (default 100 ms) and locked *every* WAL shard of that
  store to ask whether it was dirty. With the default 16 ingest shards that is
  ~1.5M lock operations per second on a 9,382-index node, for an answer that is
  always "no" on an idle index. WAL fsync is now scheduled by one process-wide,
  event-driven pool (`(cores/2).clamp(2,8)` threads, spawned lazily): a shard
  arms itself when a write arrives, and **an index with no pending writes costs
  nothing — no thread, no wakeup**.

  Measured on a real 9,382-index corpus, idle, with zero clients (10 samples ×
  15s): threads **9,709 → 339**; CPU **718-760% → 59.7-68.3%**; context
  switches **~197,000/s → ~4,000/s**; interrupts **~142,000/s → ~7,600/s**.
  Thread count no longer scales with index count — 20 indices versus 200 is 335
  threads versus 335, where it was 365 versus 541.

  Durability is unchanged and was verified with `strace -f -y -tt` so WAL
  fsyncs are distinguishable from snapshot fsyncs: `sync` fsyncs before ack,
  `batched` issues exactly one fsync inside the `wal_batch_ms` window, `async`
  issues none, and `kill -9` plus restart recovered every acked document in all
  three modes. RSS is a smaller win and reported as such: marginal 0.699 → 0.682
  MB per index (~12 KB/index of thread stack), since per-index RSS is dominated
  by index state rather than the thread. (#334)

  Not fixed here: ~0.65 core remains on that node from node-global sampling and
  O(indices)-per-tick work on shared timers — a different mechanism, tracked
  separately.

### Security

- **`cargo-audit` and `cargo-fuzz` now actually run in CI — they were only ever
  documented** ([#207](https://github.com/xerj-org/xerj/issues/207)).
  `user-feedback/09-security/cves-and-vulnerabilities.md` listed "`cargo-audit`
  for dependency vulnerability scanning" and "fuzz testing (cargo-fuzz) on all
  input parsing paths" as part of XERJ's answer to Elasticsearch's CVE history.
  Neither existed: no audit job, no fuzz job, no fuzz target anywhere in the
  tree. A reader comparing the two engines priced in a fuzzed parser surface
  that was not there.
  - New CI job `security-audit` runs `cargo audit` on every push and pull
    request; a RUSTSEC vulnerability advisory fails the build.
  - New CI job `fuzz` builds and runs seven libFuzzer harnesses
    (`engine/fuzz/`) over checked-in seeds: the Elasticsearch query DSL, the
    Lucene `query_string` grammar, date math and date-format patterns,
    index-name date math, SQL, Painless scripts, and the aggregation engine's
    two separate script tokenisers. `.github/scripts/fuzz-smoke.sh` is the same
    entry point locally and in CI.
  - `xerj-engine/tests/security_tooling_claims.rs` fails if a documented tool
    stops running, if a fuzz target ships without a seed corpus, or if the
    "all input parsing paths" overclaim reappears in any prose file.
  - The claim itself is now specific about what is and is not covered.
  - **Known gap, stated because the point of this entry is not to overclaim
    again:** seven harnesses are seven parsers, not the engine's whole input
    surface, and one parser outside them is already known to abort.
    `aggs::parse_time_zone_offset` slices a `time_zone` string at byte 2 after
    a *byte*-length check, so a `date_histogram` with `"time_zone": "+中a"` in
    an unauthenticated `_search` body dies on a character boundary. It is
    pre-existing, is not touched by this change, and is tracked in
    [#272](https://github.com/xerj-org/xerj/issues/272) with a fix and a
    proposed harness for the aggregation date parsers.

- **Fixed two high-severity advisories in the document-ingest XML parser.**
  Turning the audit gate on found `quick-xml` 0.36.2 in `xerj-autoindex`, which
  parses `.docx`, `.xlsx` and `.xml` files supplied by whoever owns the folder
  being indexed: RUSTSEC-2026-0195 (unbounded namespace-declaration allocation →
  memory-exhaustion DoS, CVSS 7.5) and RUSTSEC-2026-0194 (quadratic run time on
  duplicate attribute names, CVSS 7.5). Upgraded to 0.41, which also
  de-duplicates the crate — 0.41 was already in the tree via `pdf_oxide`.
  `crossbeam-epoch` moved 0.9.18 → 0.9.20 for RUSTSEC-2026-0204.

### Fixed


- **Following the documented setup and running `xerj autoindex` failed with a
  401.** `AuthConfig::default()` is `enabled: true`, so copying the shipped
  config as the docs instruct produced an auth-enabled server, while the pages
  that show `autoindex` all pass `--insecure` and the pages that tell you to
  write a config never mention a key. `ping()` also discarded the HTTP status,
  so the 401 on the first round trip was swallowed and the run printed ~15 lines
  of healthy-looking progress before dying at the embedding-identity probe with
  a message about the server's *capabilities* — leading the reporter to conclude
  their server lacked a feature rather than a credential. `ping()` and
  `embedding_execution_identity()` now fail fast and actionably on 401/403,
  `autoindex` gained the `admin.key` fallback `brain` already had (loopback
  only), the server sends `WWW-Authenticate: ApiKey realm="xerj"`, and the 401
  body names the real data dir plus `XERJ_API_KEY`, `--api-key` and
  `--insecure`. Reported by a user on macOS. (#333)

- **`xerj-done` reported an identical success line whether or not any source
  code was indexed.** A corpus in which every code file was junked printed the
  same terminal record as a healthy one, which is how a silently degraded
  index went unnoticed. The terminal line and run document now carry
  `code_files`, `code_files_indexed` and `code_files_junked` on both the
  generated and legacy paths, and a warning is emitted before the terminal line
  when code files were detected and none produced an indexed record. Named
  `code_files_indexed` rather than `ast` deliberately: a code file with no
  captured symbols still indexes correctly, so `ast` would overstate. (#294,
  #316)


- **The licence detector was wrong on 2 of the 13 repositories it had
  classified.** `sonic` was recorded as `GPL` when it is **MPL-2.0** — MPL-2.0
  defines "Secondary License" in terms of the GNU GPL inside its own text, and
  restrictive-first *body* matching hit that first. `quickwit` was recorded as
  `Apache-2.0/UNKNOWN` when it is plain **Apache-2.0** — `LICENSE-3rdparty.csv`
  is a dependency inventory, not the project's licence. The classifier now
  reads the title line before the body and skips third-party inventories.
  Elasticsearch's triple licence still resolves to `AGPL` from its title, so
  the safe direction is preserved where it matters most; that is a test.

- **A corpus manifest could write outside the corpus directory and destroy an
  unrelated checkout.** `--from` is documented as "rebuild a corpus someone
  else defined", so a manifest is untrusted input — but only the *corpus* name
  was screened, while the per-repo `repo` field, from the same file, was used
  directly as a path. A manifest with `"repo": "../../../work/repo"` moved that
  checkout to the manifest's commit, discarded its uncommitted changes and
  untracked files (`checkout --force`, `clean -fd`), and rewrote its `origin`
  remote. `repo` must now be a plain directory name, enforced in `xc-corpus.sh`
  and in `validate_manifest.py`, with the destructive path double-guarded.

- **`xc-index.sh` silently ignored an unrecognised argument.**
  `xc-index.sh <corpus> --frsh` set no flag, ran an *incremental* index and
  exited 0. Because `autoindex` keeps incremental state in `~/.xerj/autoindex/`,
  a run that should have been fresh skips every file as "already indexed" and
  leaves a corpus with 0 documents that retrieves nothing, reporting no error
  anywhere. Unknown arguments are now rejected by name.

- **`xc.py` warned about only `GPL` and `LGPL`, by exact match**, so it was
  silent on every restricted repository the hub deliberately ships:
  elasticsearch (AGPL/SSPL/Elastic), meilisearch's BUSL Enterprise Edition
  parts, and — once the detector above was corrected from `GPL` to `MPL-2.0` —
  sonic, which had been warning before. The warning at retrieval time is the
  one that counts, because that is when an agent is looking at the code and
  deciding whether to lift it; it now covers the same set as `xc-corpus.sh`.

- New CI job `reference-coding`: shell and Python syntax, the offline
  round-trip suite (`tools/xerj-code/tests/test_xc_corpus.sh`, 28 checks, local
  git repositories over `file://`, never touching the real `~/.xerj-code`), and
  `validate_manifest.py --hub` over every shipped manifest.

- **A binary that does not understand `cluster_state.json` now fails closed
  before activating storage.** The previous loader accepted future and
  partially-understood shapes, dropped fields it did not know, and could later
  rewrite the reduced document over the original. Boot now accepts only the
  complete shipped format-1 envelope, including duplicate-key and nested-shape
  checks. A rejected document leaves liveness at 200 and readiness at 503, but
  opens no user index, Console system index, WAL, segment, or durable audit
  sink; Console bootstrap and storage background jobs remain disabled until a
  supported restart. After the existing request-size, authentication, and
  authorization gates, HTTP storage access returns the stable
  `cluster_state_unavailable` 503; authenticated gRPC calls return
  `UNAVAILABLE`. Existing 401/403/body-limit precedence is unchanged. Client
  responses give the category and recovery action without a local path or
  persisted object names; the server log carries the detailed classification
  and path for the operator. The original bytes and any legacy staging file
  stay in place, and a blocked boot creates no salvage copy. Format-1 rewrites
  retain the existing
  same-directory temp-file fsync and atomic rename; parent-directory sync is a
  best-effort attempt whose errors are ignored, so this change does not claim
  strict namespace durability across power loss. Data-directory creation,
  `node.lock` acquisition/PID diagnostics, and the server's earlier
  credential/TLS preparation retain their existing behavior; the no-open/no-
  replay claim begins at the cluster-state classification and covers storage
  stores, not those process-bootstrap files.

- **`PUT /{index}/_settings {"index.lifecycle.name": null}` actually detaches
  the index, and the detach survives a restart** (#282, ported from #262;
  regression-tested in [#290](https://github.com/xerj-org/xerj/pull/290)). The
  null previously fell through a string-only settings reader, so the operator
  got `200 acknowledged` while the index stayed attached — to a policy whose
  delete phase then destroyed it. A detach now removes the execution cursor,
  writes a persisted tombstone (`ism_managed_indices.json` grew a
  `managed`/`detached` envelope; the old bare-map file is still read), and
  scrubs the stale `index.lifecycle.name` from the stored settings.
- **The lifecycle delete action gained #262's safety rails.** It now refuses —
  visibly, in `explain`, never silently — to delete a dot-prefixed internal
  index (`.ds-*` backing indices exempt), a data stream's current write index,
  or an index whose age cannot be established from its execution cursor.
- **An ILM policy naming an action the engine cannot execute is refused at PUT
  time with the action named** (400), instead of being stored with the action
  silently dropped — the accepted-and-ignored class (#204). Executable ILM
  actions are `rollover`, `delete`, `readonly`.
- **`autoindex` no longer rejects a resumed generation after journal replay
  when inferred floating-point schema statistics need exact decimal
  round-tripping.** Unchanged reruns and junk-bearing corpus shrink now replay
  correctly; existing affected journals resume without a rebuild, while the
  manifest format and integrity checks remain unchanged.

- **A `query_string` containing one non-ASCII character aborted the process.**
  `{"query_string":{"query":"_\u0660"}}` — or the same text in `?q=` — took the
  server down. The Lucene tokeniser walks bytes but slices `&q[start..i]` as a
  `&str`, and it broke on `is_whitespace()`, which is true for Latin-1 NBSP
  (0xA0). In valid UTF-8 that byte is only ever a *continuation* byte, so the
  scan stopped between the two bytes of U+0660 and the slice panicked on a
  character boundary. Every delimiter test in that scanner is now `is_ascii_*`,
  which makes "the scan can only stop on a character boundary" true by
  construction. Nothing is lost: the only non-ASCII bytes the old test accepted
  were 0x85 and 0xA0, neither of which is a standalone character in UTF-8.

- **An index name of `<{}{}>` aborted the process.** Index-name date math finds
  the first `{` and the last `}` inside the braces; when the `}` comes first,
  `date_part[2..0]` is a panic, not an empty slice. Reachable from any request
  that names an index. The closing brace is now required to come after the
  opening one.

- **A Painless script containing one non-ASCII character aborted the process.**
  The second thing the new fuzz harnesses found, inside a second. The tokeniser
  walks *bytes* (`bytes[i] as char` reinterprets each byte as Latin-1) but
  slices `&src[start..i]` as a `&str`. Every continuation byte of a multi-byte
  character reads as an alphabetic Latin-1 char, so an identifier scan could
  start or stop inside one and the slice panicked on a char boundary —
  `end byte index 24 is not a char boundary; it is inside 'ʋ'`. Scripts arrive
  in ordinary search and update bodies and the release profile sets
  `panic = "abort"`, so this was not a 400, it was the whole server. The
  identifier scan is now ASCII-only, which keeps both ends on a character
  boundary by construction; a non-ASCII byte reaches the tokeniser's
  `unexpected char` error instead. String literals still carry any Unicode.

- **A search body could abort the whole process: `{"range":{"ts":{"gte":"now+33333333333333H"}}}`.**
  The first thing the new `date_math` fuzz target found, 20 seconds into its
  first run. `chrono`'s `Duration::hours` and its siblings **panic** when the
  count does not fit a `TimeDelta`, and they run before the
  `checked_add_signed` that was supposed to make
  `xerj_query::dates::add_unit` total — so the careful checking around them
  bought nothing. A range bound goes straight from the request body into this
  code (`parser.rs:931`), the release profile sets `panic = "abort"`, and the
  result is an unauthenticated remote denial of service on a released binary.
  Fixed with the fallible `try_*` constructors. Grepping for the same pattern
  found **three** more copies in the index-name date-math resolvers —
  `xerj-engine/src/index.rs`, and *both* branches of
  `xerj-api/src/es_compat.rs`: the `now` form (`<logs-{now+9999999999999d}>`)
  and the anchored form (`<logs-{2026-01-01||+9999999999999d}>`), which are
  handled by two different functions sixty lines apart. All three are fixed,
  their `n * 30` / `n * 365` multiplications are `checked_mul`, the unit is read
  as one character rather than one byte, and the addition goes through
  `checked_add_signed`. `es_compat`'s resolver is the one that runs on the wire
  — the index path parameter of every create/index call and the `_search` index
  list — so `crates/xerj-api/tests/index_name_date_math_is_total.rs` now drives
  it at the crate boundary; the same-file fix that missed the anchored branch
  passed because nothing exercised the public entry point.

- **A `_search` aggregation script containing one non-ASCII character aborted
  the process.** `painless::tokenize` was not the only Painless-subset scanner
  in the tree: `aggs.rs` holds two more with the identical byte-walk /
  `&str`-slice shape — `lex_script` (`scripted_metric`) and `tokenize_script`
  (`bucket_script` / `bucket_selector`) — and both took `&src[i..i + 2]` to test
  for a two-character operator at a byte index that can sit inside a multi-byte
  character, because every arm above it is ASCII-only.
  `{"m":{"scripted_metric":{"map_script":"中"}}}` was `end byte index 2 is not a
  char boundary`. Both also skipped whitespace with `is_whitespace()` on
  `bytes[i] as char`, which is true for Latin-1 NEL and NBSP — bytes that in
  valid UTF-8 are only ever *continuation* bytes. The two-character probe now
  requires both bytes to be ASCII (every such operator is ASCII, so nothing is
  lost) and whitespace is `is_ascii_whitespace`. A new `agg_script` fuzz target
  covers all three entry points, which is how the next one gets found instead of
  reported.

- **An XML entity nobody declared no longer discards the text around it.**
  Fallout from the `quick-xml` upgrade above, which is a breaking change to how
  character data arrives: 0.38 stopped folding `&amp;` into the surrounding
  `Event::Text` and began emitting each reference as its own
  `Event::GeneralRef`. Entity handling is preserved across that change (a
  reader that ignored the new event would have silently dropped every `&`, `<`,
  `>` and `&#233;` while the words around them still arrived), and one case
  gets better: 0.36's `BytesText::unescape()` failed the whole text node on an
  entity no DTD declared for us, and `unwrap_or_default()` then discarded it —
  now only the reference itself is dropped. Character data is also reassembled
  from its fragments before being emitted, so an entity-bearing field stays one
  value instead of becoming an array of pieces.

- **`constant_score` on a mixed flushed/unflushed corpus returned the wrong
  page, and any tied hit set could be re-sorted into `_id` order**
  ([#270](https://github.com/xerj-org/xerj/issues/270),
  [#300](https://github.com/xerj-org/xerj/pull/300)). Two defects. A
  top-level `constant_score` (or `boost`) wrapper defeated the match-all
  shortcut, so the stored scan ran in exact-counting mode and a bounded page
  came back all-memtable — `size:1` on 40 flushed + 300 unflushed identical
  docs returned the memtable doc where the oldest flushed doc was correct;
  the wrapper is now peeled for the count/bounds decisions only, with scoring
  unchanged. Separately, five post-sort re-sorts — the bool-text IDF rescore,
  the near-zero-BM25 TF-IDF fallback, and the three `rescore` sorts — tied by
  `_id` alone, so any of them firing on tied scores reordered the page; the
  main sort and all five now route through the one page-order key
  (`score DESC, seq_no ASC, _id ASC`). Known residual, documented rather
  than asserted around: bounded pages for multi-clause bools on a mixed
  corpus are still admitted under first-pass scores (#188's remit).

- **A shrinking file set on a junk-bearing `autoindex` generation aborted the
  cutover with `desired manifest digest does not match sync_begin payload`**
  ([#283](https://github.com/xerj-org/xerj/issues/283),
  [#296](https://github.com/xerj-org/xerj/pull/296)). A fresh plan kept
  duplicate-file aliases whose content was junk or skipped while the
  incremental projection dropped them, so the two disagreed on byte-identical
  junk duplicates — two empty files suffice. The fresh plan now projects
  exactly like the incremental path, so the shrink reconciles cleanly. Where
  a journal is *genuinely* inconsistent, the replay and validation paths now
  say what to do — no remote data changed, re-running will not repair it,
  rebuild with a new `--state-dir` and `--prefix` — with the invariant kept
  as the error cause.

- **An outer repository's `.gitignore` no longer judges files inside a nested
  checkout** (the second rc.15 known issue below;
  [#287](https://github.com/xerj-org/xerj/pull/287), repairing
  [#279](https://github.com/xerj-org/xerj/pull/279)). The rule walk had no
  repository-boundary stop, so a root `*.md` hid the README of every
  vendored dependency beneath it. The git-sourced rule kinds now stop at a
  `.git` boundary — the set kept under a nested checkout is byte-for-byte
  what `git status --untracked-files=all` reports there — while
  `.xerjignore` and the built-in defaults deliberately still govern the
  whole folder you named. Three more defects from the same merge: hidden
  directories pruned by the dotfile rule were deep-counted into
  `ignored_files_in_pruned_dirs` (97,731 phantom files on this repository's
  own walk); that number reached the `xerj-done` line and `--progress json`
  as a bare total when it is budget-capped — a new
  `ignored_files_in_pruned_dirs_exact` flag now says whether it is a total
  or a floor; and `--no-ignore`/`--no-default-ignores` were accepted and
  ignored (#204) on `autoindex map` and `status`, which never walk a
  filesystem — both are now refused there with the reason. The shipped docs
  that contradicted the code are corrected.

- **A finished `autoindex` run could leave its state-directory lock held, so
  the immediately following run on the same state dir was refused as
  already in use** ([#305](https://github.com/xerj-org/xerj/pull/305)).
  `flock` ownership follows the open file description, so a helper
  subprocess forked while the journal was live (the PDF extraction helper,
  git invocations) could inherit the descriptor and keep the lock held past
  the owner's close. The lock guard now records the acquiring PID and
  unlocks on drop only in that process — a fork-inherited copy closes its
  descriptor without releasing the live owner's lock. No retry, wait or
  takeover behaviour was added; crash recovery is unchanged. Covered by a
  same-OFD unit oracle and a real-fork integration regression on
  Linux/macOS.

- **`autoindex` indexed a `.mts` or `.cts` file as prose**
  ([#284](https://github.com/xerj-org/xerj/pull/284)). The extension
  registry claimed `.mjs`/`.cjs` but not TypeScript's own ESM/CJS
  counterparts, and an unclaimed extension does not degrade to "code without
  symbols" — it fell through to the prose extractor, so the file carried no
  `language`, no `defs`, no `symbols`, was split into several body-only
  records, and was invisible to symbol search.

- **An exported module `const` reached `defs` in neither TypeScript nor
  JavaScript** ([#285](https://github.com/xerj-org/xerj/pull/285),
  [#304](https://github.com/xerj-org/xerj/pull/304), closes
  [#293](https://github.com/xerj-org/xerj/issues/293)). Capture only fired
  when the value was a function, so a module whose whole public surface is
  built by factory calls — `export const users = pgTable(...)`, a router, a
  config object — extracted zero symbols and could not be found by symbol
  search at all. The new pattern is exported-only, anchored to module scope,
  and matches the `const` token, so function-local bindings and mutable
  exported `let` stay out; module-private `const` was measured and excluded
  because it added weight without retrieval gain.

- **A PHP 8.1 enum extracted zero symbols, and class constants were never
  captured** ([#286](https://github.com/xerj-org/xerj/pull/286)). An enum
  declares no class, interface or trait, so a file holding one could not be
  found by its own name through symbol search. Enums, enum cases and `const`
  declarations — class-level and file-scope — now land in `defs`, with
  cases treated as named constants the way Go's `const_spec` members are;
  `define()` calls stay uncaptured.

### Added


- **`xerj mcp` — the MCP server now ships in the installed binary.** XERJ had a
  complete, tested 10-tool MCP server that no user could obtain: releases built
  only `xerj-server`, the installer shipped only `xerj`, and MCP appeared in no
  agent-facing doc. It is now a subcommand, so anyone who ran
  `curl -fsSL https://xerj.org/get | sh` gets it on their next upgrade, with no
  installer or release-matrix change and no new third-party dependencies. Tools:
  `xerj_search`, `xerj_semantic_search`, `xerj_vector_search`,
  `xerj_hybrid_search`, `xerj_memory_store`, `xerj_memory_recall`,
  `xerj_brain_ego`, `xerj_brain_link`, `xerj_brain_unlink`,
  `xerj_brain_overview`. A XERJ node must already be running; `xerj mcp` does
  not start one. The published tool schema at
  `/docs/agents/schemas/mcp-tools.json` had drifted to six tools against ten
  served — it is now generated from a real `tools/list` and gated in CI against
  drifting again.

- **`[profile.quick]` and `[profile.ci-test]`** for the build and test halves of
  the verify loop. `[profile.release]` is `lto = "fat"` + `codegen-units = 1`,
  deliberately the slowest build Rust can produce because it yields the fastest
  binary — correct for a shipped artifact, wrong for "does this compile?". CI's
  test job spent 76m05s compiling for 1m52s of tests. Neither profile is ever
  valid for a released artifact or a published performance number.
  `docs/FAST_BUILDS.md` documents the remaining levers.


- **The reference-coding toolkit ships in the repo, and a corpus definition can
  now rebuild a corpus** ([#319](https://github.com/xerj-org/xerj/issues/319)).
  `xc-corpus.sh`, `xc-index.sh`, `xc.py` and `SKILL.md` lived only in a
  gitignored working copy, so `git ls-files | grep xc-` returned nothing while
  the case study, `CONTRIBUTING.md` and the landing page all told a reader to
  run them. They are now in [`tools/xerj-code/`](tools/xerj-code/), with a
  `!tools/**/*.md` re-include in `.gitignore` — the blanket `*.md` rule was what
  swallowed `SKILL.md`, the same failure mode that hid this repo's pull-request
  template for its entire history.
  - `corpus.json` records the **full 40-character SHA**. The old
    `rev-parse --short` value cannot be fetched from a remote
    (`git fetch --depth 1 origin e449d17` → "couldn't find remote ref"), so a
    manifest could not pin anything. A manifest carrying a short SHA is now
    rejected with a SHA-specific error rather than silently rebuilt at the tip.
  - `xc-corpus.sh --from <manifest>` rebuilds a corpus at its pinned commits —
    fetch-by-SHA at depth 1 with progressively deeper fallbacks — and an
    existing clone is *moved* to the recorded SHA instead of skipped.
  - [`tools/xerj-code/hub/`](tools/xerj-code/hub/) carries vetted definitions
    for the four corpora this repo uses. Each entry has a `review` block a
    human filled in: SPDX expression, `adapt-with-attribution` /
    `approach-only` / `mixed`, reviewer and date.
  - Hosting pre-built *indexes* remains out of scope (on-disk format version,
    embedder identity, redistribution of the indexed source), as does the
    measurement harness — `CASE_STUDY.md` and `SKILL.md` previously pointed at
    a `MEASURE.md` that is not in the repo, and now say plainly what ships.

- **`GET /_ilm/status`, `POST /_ilm/start`, `POST /_ilm/stop`** (#282): the
  operator can halt lifecycle execution without stopping the node; a stopped
  engine's tick touches nothing. The flag is in-memory — a restart resumes
  execution — while the per-index detach tombstone is the durable stop.
- **`POST /{index}/_ilm/remove`** (#282): ES's own detach verb; goes through
  the same tombstoned detach path as the settings-null route. A literal name
  that is not an index answers 404 rather than writing a tombstone for a
  ghost.

- **A dynamically-discovered nested object field is no longer permanently
  opaque to `GET _mapping`/`_field_caps`**
  ([#292](https://github.com/xerj-org/xerj/pull/292),
  [#307](https://github.com/xerj-org/xerj/pull/307)). A subfield like `metadata.kind`
  was already queryable (dotted-path resolution against `_source` doesn't
  care about the mapping) but had no way to be *discovered* by a client that
  reads the mapping instead of already knowing the field name — indistinguishable
  from not existing to Kibana/OSD's own field-list UI. Three compounding gaps,
  all fixed together: dynamic mapping never recursed into `Value::Object`
  (`{"type": "object"}` was a terminal, not real ES/OS default behavior);
  `GET _mapping`/`_field_caps` served the index-creation-time explicit mapping
  blob verbatim forever, so *any* field discovered dynamically after creation
  — flat or nested — never surfaced, no matter how many documents arrived; and
  `_field_caps`'s per-field walk only ever registered top-level names, so a
  nested object's own children were still invisible even once the mapping
  correctly described them. Measured end-to-end on `/_memory`'s own backing
  index: `GET .xerj-memory-{ns}/_field_caps?fields=metadata.kind` returned
  `{"fields": {}}` before, the real entry after. The cluster-wide
  `GET /_field_caps` (as opposed to the per-index `GET /{index}/_field_caps`)
  had the same gaps independently and is fixed the same way — the two no
  longer disagree about which fields exist. Dynamic mapping also now
  re-inspects nested fields on *every* document — a later document adding a
  new nested key merges it into the existing mapping instead of leaving it
  permanently invisible — and `Schema::field_count()` counts nested children
  and multi-fields recursively, closing a `max_fields_per_index` bypass
  where one top-level object could carry unbounded uncounted children. Note
  the tightening this implies: a dynamic text field with its `.keyword`
  multi-field now costs 2 against the default limit of 500, which matches
  how ES counts `index.mapping.total_fields.limit`. Two bounded residuals
  from review (a one-shot budget overshoot on a single deeply-keyed insert;
  array-nested keys deferred to the 100-doc recheck) are filed as
  [#312](https://github.com/xerj-org/xerj/issues/312).
  **Known limitation**: this only benefits indices created after this
  ships. An index (a `/_memory` namespace or otherwise) created before it
  already has its stored mapping written, and that isn't retroactively
  edited — existing indices need either re-creation or an explicit
  `PUT _mapping` to pick up the fix. No automatic migration is included.

- **A crafted filename can no longer forge records on the agent progress
  stream** (the first rc.15 known issue below). Every externally-controlled
  string — in-flight paths, the paths and error text interpolated into human
  notes, the terminal line's reason — is stripped of control characters, bidi
  overrides and zero-width characters and bounded in length before it reaches
  any progress surface. Measured on a corpus holding crafted names: a run that
  previously emitted a forged `xerj-done ok=true exit=0 reason=completed`
  2.2 s ahead of its real terminal line now emits one terminal line and no
  control characters at all. The same sanitisation covers the walker's
  "skipping unreadable entry" warning, which renders a path to the same stderr.
- **`--progress json` paces the bar like `--progress plain`.** The `bar` field
  bypassed the spacing slot entirely: measured at `--progress-interval 1`, 178
  of 178 ticks carried a bar against 18 bars on the plain surface. It is now a
  string on exactly the ticks that owe a bar and `null` in between — 26 of 320
  ticks on the same corpus, against 16 on plain.
- **The bar spacing floor is the 15 s the documentation states.** A half-tick
  tolerance made the enforced floor `interval/2` shorter — 12.5 s at the
  shipped defaults. Measured at `--progress-interval 10`: 11 of 11 same-phase
  gaps under 15 s (min 10.0 s) before, none after (min 19.3 s).
- **`/{index}/_refresh` reports segment-publication failures instead of
  claiming every shard succeeded.** The endpoint previously discarded every
  error returned by the synchronous engine flush and always answered HTTP 200
  with `successful == total`. It now includes the index, status, and underlying
  reason in `_shards.failures`, attempts every resolved index, and returns the
  first failed shard's HTTP status as Elasticsearch 8.13.4 does for refresh.

- **Text field names containing path separators can be flushed and reopened.**
  FTS side-car filenames previously interpolated the raw field name, so a name
  such as `styles_blocks_core/site-title` became a child path and prevented the
  segment from being published. Unsafe or overlong field names now use one
  deterministic bounded filename component. Existing portable names retain
  their byte-identical paths. Each segment has an immutable filename-layout
  discriminator: absence means the historical raw layout; an explicit marker
  means the encoded layout. Readers never infer the layout from side-car file
  existence, preventing partial files and raw names that resemble digests from
  aliasing another field. Before the discriminator or first encoded side-car
  is written, the index durably advances its data-directory marker to format 2;
  older binaries refuse that index instead of silently missing the new layout.
  A retry after a post-rename marker failure must re-establish durable marker
  publication before writing the discriminator or encoded FTS paths. Unix
  confirms the parent-directory fsync; Windows uses a same-directory Win32
  write-through replacement rather than claiming its directory-sync no-op is
  durable. Safe-only indices remain format 1.

### Changed

- **Default `_search` source payloads omit engine-generated embedding
  companions** ([#309](https://github.com/xerj-org/xerj/pull/309)). A search
  with no `_source` now returns ordinary source fields without
  `<field>_vector` and `<field>_vector_chunks` — identified from the
  mapping's embedding config, not by name; measured payload size fell from
  211,432 to 21,069 bytes for 5 hits (about 90%). Explicit `"_source": true`,
  `"_source": ["field"]`, and include/exclude forms are unchanged, vectors
  remain stored and used for kNN/semantic/hybrid scoring, and the
  document-copying paths (`_reindex`, index clone, enrich execution) request
  the full source explicitly so copies keep embeddings byte-for-byte. Add
  `"_source": true` to restore the previous complete payload, including
  vectors. Two residuals disclosed from review: a `fields` request naming a
  companion resolves against the already-filtered source and comes back
  empty under the new default — name it in `_source` instead
  ([#310](https://github.com/xerj-org/xerj/issues/310)) — and the new
  default arm copies each hit's source even on indices with no embedding
  fields, a constant-factor read-path cost filed as
  [#311](https://github.com/xerj-org/xerj/issues/311).

- **`merge.strategy = "log_structured"` is now refused at startup instead of
  silently running the size-tiered policy**
  ([#207](https://github.com/xerj-org/xerj/issues/207)). Nothing in the tree
  ever read `merge.strategy`; `run_merge_once` always builds a
  `SizeTieredMergePolicy`. An operator who chose a levelled policy for its read
  amplification got the other one, quietly. This follows the `storage.backend`
  and `vector.default_quantization` guards already in `Config::validate`.

- **The three dormant `[merge]` settings now say so, in the code, in the shipped
  TOML, on the docs site, and in the log at startup**
  ([#207](https://github.com/xerj-org/xerj/issues/207)). The issue reported
  merge I/O rate limiting as "claimed but dormant" and it is: the `RateLimiter`
  that honours `io_rate_mb_per_sec` is wired only into `xerj-storage`'s legacy
  `MergeExecutor`, which the engine never constructs. `min_segments` and
  `max_concurrent` are unread too — the real per-tier trigger is
  `min_merge_count`, and merge parallelism comes from `XERJ_MERGE_PARALLELISM`.
  `MergeConfig::dormant_overrides` names any of the three the operator has moved
  off its default and `xerj-server` logs each at WARN, so the operator throttling
  merges to protect query latency finds out instead of guessing. They stay
  accepted rather than becoming hard errors because the wrong value costs
  latency, not data, and `io_rate_mb_per_sec = 100` has shipped in
  `xerj.default.toml` since v0.1. Wiring a real throttle into `run_merge_once`
  is the fix that makes this list shorter.

- **`engine/xerj.default.toml` disagreed with the defaults it claims to
  document, in four places.** `max_segment_mb` said 5120 against a real 8192,
  `wal_max_size_mb` 512 against 1024, `flush_size_mb` 256 against 512, and
  `default_quantization` `"scalar8"` against `"none"` — so `cp xerj.default.toml
  xerj.toml`, the documented first step, silently changed four engine behaviours
  including turning on 8-bit vector quantization. The file is corrected and
  `shipped_default_config_documents_the_real_defaults` now diffs every leaf key
  against `Config::default()`, so it cannot drift again.

- **The docs site had drifted the same way, and now has the same guard.**
  `landing/docs/config.html` shipped `wal_max_size_mb = 512`,
  `flush_size_mb = 256` and `default_quantization = "scalar8"` in its
  copy-pasteable `[storage]` and `[vector]` blocks while its own DEFAULT
  table — thirty lines up, on the same page — said 1024, 512 and `"none"`;
  `landing/docs/storage.html` said the WAL rolls at 512 MiB;
  `landing/docs/vectors.html` called `scalar8` "the default" and listed a
  `scalar4` mode that no config or mapping value can reach; and
  `engine/README.md` put `flush_size_mb` at 256 MiB. All four are corrected,
  along with the docs search-index blob duplicated across 44 landing pages,
  which offered `scalar4` as a selectable quantization mode.
  A second guard, `the_docs_site_config_page_agrees_with_the_real_defaults`,
  now diffs *both* halves of that page — every DEFAULT cell in the reference
  table and every assignment in the example blocks — against
  `Config::default()`. An example that deliberately differs (the blocks whose
  point is switching TLS or cluster mode on) has to carry `# not a default` on
  the line, so the reader is told as well as the test.
  - **Known gap, found while correcting the `scalar4` prose and disclosed
    rather than quietly fixed:** the guard reads `config.html` against
    `Config::default()`, so it covers the *config* half only. Nothing checks a
    *mapping*, and a mapping is not validated either —
    `es_compat.rs` matches `"scalar8" | "int8" | "none"` on a `dense_vector`
    field's `quantization` and falls through `_ => {}`, so
    `"quantization": "scalar4"` (or `"binary"`, or a typo like `"sq8"`) is
    accepted with a 200, echoed back verbatim by `GET /_mapping`, and ignored
    — the field is stored at full-precision f32 while the mapping says it is
    quantized. Measured through the ES-compat router at this commit. It is
    pre-existing and outside this diff; it is another instance of the
    accepted-and-ignored class in
    [#204](https://github.com/xerj-org/xerj/issues/204), is filed with the
    repro and a proposed 400 as
    [#275](https://github.com/xerj-org/xerj/issues/275), and
    `landing/docs/vectors.html` now says so on the page instead of leaving
    "not a mode you can select" to imply the value is rejected.

- **Four other claims from [#207](https://github.com/xerj-org/xerj/issues/207)
  now match the source.**
  - *Settings count.* `config.rs` said 38 in its header and 56 twenty lines
    down while the count test asserted 60 — and the test asserted it by
    re-adding a hardcoded sum (`5 + 2 + 3 + … - 1 == 38`), an identity that
    could not fail whatever `Config` held. There was a second copy of the same
    non-test in `config.rs` itself (`count_user_facing_settings`, `61 == 61`).
    Both now serialise `Config::default()` and count leaf keys, section by
    section. The measured figure is **105** (103 when this work was done; the
    rc.14 `server.allow_insecure_network_bind` key made it 104 and #247's
    `lifecycle.tick_interval_secs` makes it 105 — which is the point: it is
    measured, so it moves with the code), and 38 / 50 /
    56 / 60 / 61 / "<50"
    are corrected in `config.rs`, `xerj-common/src/lib.rs`, `engine/README.md`,
    `xerj.default.toml` and the feedback responses, along with the stale
    per-sub-config annotations (limits 3 → 13, storage 5 → 10, embedding 4 → 19,
    merge 5 → 8, cluster 4 → 5, tls 3 → 4, auth 2 → 3). This closes item 10 of
    `user-feedback/ROADMAP-TO-GA.md`. 105 versus 3,000+ is still the winning
    story, told truthfully.
  - *Wire-compatibility test framing.* `journey_es_migration` was described as
    proving "the same curl commands" work. It calls the Rust engine API
    directly and never makes an HTTP request. The response-shape assertions are
    real and stay; the comment now says what the test does and points at the
    ES-compat YAML conformance suite, which is what actually covers the wire.
  - *OpenAPI spec.* `landing/openapi.json` declared `version: 1.0` while
    specifying 7 routes out of the 200-plus the router registers. It is now
    titled and versioned as a partial spec, and says so.

- **`autoindex` estimates the job on the user's own machine and hands the
  decision back before it takes their laptop.** Phase A already reads and
  parses every file to sniff and sample it, so it now *times* that work per
  format family and turns it into a range with its basis printed next to it —
  `code 500 files 749.3 MB at 11.7 MB/s measured over 500 file(s) → 64.2 s` —
  rather than a constant calibrated on someone else's hardware. A family is
  priced only from files phase A **provably** read end to end (whole-file
  parsers always; streaming parsers only when the file was under the sampling
  byte cap *and* stopped short of the record cap; never `sqlite`, never
  gzip), and everything else is named under `unmeasured_families` with its
  bytes instead of being priced at another family's rate. If nothing could be
  measured there is no number at all and no gate. The two ends are the
  classical list-scheduling bounds (Graham, 1969) over the phase-B worker
  count that #240's resource policy chose.
  The number is a **floor**, labelled as one on every surface: it covers
  client-side extraction only, because measuring the server, the network or
  embedding would mean writing to the index the estimate exists to ask
  permission for. Measured on a 68 MB source tree the floor was 0.1 s against
  a real 8.9 s run; on a 793 MB one, 64.2 s against ~350 s. The gate therefore
  under-asks and never over-asks, and says so where a reader would otherwise
  mistake silence for a promise.
- **`--max-minutes` (default 10) stops the run and asks instead of deciding.**
  Past the threshold with no answer, autoindex indexes nothing, writes a JSON
  decision request to stdout and exits **4** — a code of its own, because exit
  1 is already the catch-all for every real failure and an agent must be able
  to tell "choose something" from "your endpoint is down". The payload carries
  the estimate and its basis, file/byte counts, the per-band work order, the
  heaviest directories with real byte counts (flagged when they match the
  vendored/generated rule), and four options: `proceed`, `fast`, `narrower`,
  `cancel`. Answer with `--approve <id>` (`--yes` = proceed), which skips the
  gate; `--max-minutes 0` disables it. A person at a terminal gets the same
  facts as prose plus a prompt; a piped or agent-driven run is **never**
  blocked on stdin, and an unanswerable prompt is a cancel, never consent.
  `--approve fast` really applies `--no-semantic --no-graph`, and `--approve
  narrower` is refused with instructions rather than accepted and ignored
  (#204). The `fast` option states no speed-up factor: it reports the datasets
  and file count it changes and says plainly that this run did not measure the
  factor.
  The gate governs the phase-B route. An **incremental reconcile of an already
  committed generation** (a `--no-graph` re-run over a folder that already has
  one) processes only what changed and publishes from a sealed snapshot, so
  `--max-minutes` does not apply there — and that route now *says so* on
  stderr rather than accepting the flag and quietly not honouring it.

- **`autoindex` indexes what matters first.** Phase B's queue was sorted by
  size alone — right about scheduling, silent about value, so a user who
  stopped early got whatever was largest, which on a source tree is
  `node_modules`. Work is now ordered by value band (source and documents →
  configuration → structured data → logs and line files → vendored, generated
  and minified paths), with the old biggest-first rule kept *inside* each band
  so a large file still runs alongside its band instead of becoming the tail;
  a single-worker run goes smallest-first, where there is no tail to hide in.
  One exception keeps the new order from costing wall clock: a file whose own
  extraction outlasts everything ranked above it (`size × workers > the rest`)
  starts first regardless of band. The bands, their file/byte counts and the
  reason each sits where it does are printed with the plan and included in the
  decision payload as `priority_order` — an unexplained order is
  indistinguishable from an arbitrary one. Verified live on a 28 MB mixed
  corpus at two workers: the first second of phase B was spent on
  `src/mod_*.rs`, and the 1.7 MB `node_modules/**` files — the largest in the
  corpus, and therefore *first* under the old rule — drained last.
  Bytes-based progress and its percent are unaffected: reordering does not
  change the denominator.

## [1.0.0-rc.15] - 2026-08-10

### Known issues in this release

Both were found by adversarial review *after* their pull requests merged, so
they are present in these binaries. Fixes are in flight for rc.16.

- **A crafted filename can forge records on the agent progress stream.** The
  in-flight path is rendered onto the progress surface without control-character
  sanitisation, so a filename containing a newline can inject something that
  looks like a genuine `xerj-progress` or `xerj-done` record — including a false
  `ok=true` completion — into the stream this release tells AI agents to parse.
  Indexing a repository whose filenames you do not control is enough to trigger
  it. Until the fix lands, do not treat that stream as trustworthy when the
  corpus is untrusted.
- **An outer repository's `.gitignore` judges files inside a nested checkout.**
  The ignore lookup walks every layer with no repository-boundary stop, unlike
  git itself, so a vendored or submoduled repository is filtered by rules that
  have no authority over it. Files you expect to be indexed may be skipped.
  `--no-ignore` bypasses it.

Also unfixed and worth knowing: `--progress json` does not pace the bar (a JSON
consumer sees roughly ten times as many bar lines as a plain consumer), and the
documented "at most one bar per 15 s" floor is really 12.5 s.


### Added

- **Index lifecycle policies are executed, not just stored**
  ([#199](https://github.com/xerj-org/xerj/issues/199), contributed by
  @Vinz2168). A single internal engine modeled on OpenSearch's ISM state machine
  — named states, ordered actions, ordered transitions — exposed through two
  REST surfaces: native ISM at `_plugins/_ism/*` and the Elasticsearch-shaped
  `_ilm/*`. `spawn_lifecycle_manager()` runs the tick on
  `lifecycle.tick_interval_secs` (default 300s, matching OpenSearch ISM's own
  job interval), and the managed-index cursor survives restart. Retention now
  actually deletes and rolls over instead of being acknowledged and forgotten.
  One documented divergence: `min_age` is measured from when an index entered
  its current state rather than from rollover time, so a policy that rolls over
  *and* has a downstream phase meant to be measured from the rollover will
  advance on a different clock than Elasticsearch uses.

- **`xerj autoindex` honours `.gitignore` and `.xerjignore`**
  ([#276](https://github.com/xerj-org/xerj/issues/276)). Nested ignore files and
  negation patterns follow git's own precedence, with build-output defaults
  (`node_modules`, `vendor`, `target`, `dist`, `build`, `.venv`, `__pycache__`)
  on top; `--no-ignore` and `--no-default-ignores` turn them off. Pointing xerj
  directly at an ignored path still indexes it — an explicit instruction beats a
  rule the user did not write for this purpose. `--dry-run` reports what was
  skipped and by which rule, because a user whose files did not appear needs to
  know why. Measured on this repository: 274,826 files walked in 1.18s becomes
  1,385 in 0.25s.

- **Progress is relayed to an AI agent as a drawn bar.** The stream surface
  carries a rendered bar alongside its machine-readable `key=value` fields, so
  an agent driving `xerj autoindex` on someone's machine can show a real
  progress line rather than going silent for minutes. `--progress json` keeps a
  single parseable stream; `--quiet` still writes nothing on either.

### Fixed

- **Tied scores have one total order — a bounded page is a prefix of the full
  page** ([#191](https://github.com/xerj-org/xerj/issues/191)). A `size:5` page
  and a `size:1000` page disagreed about which tied document came back. The
  memtable's bounded candidates are now ranked by the same page key as the
  segment path, so the two agree. The remaining `constant_score` and post-sort
  re-sort cases are tracked separately in
  [#270](https://github.com/xerj-org/xerj/issues/270).

- **`date_histogram` no longer aborts the process on a multi-byte `time_zone`**
  ([#272](https://github.com/xerj-org/xerj/issues/272), contributed by
  @Vinz2168). `aggs::parse_time_zone_offset` sliced a `&str` on a byte index
  that could land mid-character, and with `panic=abort` that took the whole node
  down — an unauthenticated request was enough.

- **The legacy path-parameter form of scroll continuation works again**
  (contributed by @Vinz2168), and **`_cat/*?format=json` plus `_data_stream/**`
  wildcards** are supported (also @Vinz2168) — both are shapes real
  Elasticsearch clients send.

### Changed

- **`autoindex` parses each PDF once per fresh run instead of twice.** Phase A
  already paid a *complete* parse for every PDF — `extract::extract` routes
  `Family::Pdf` straight to the isolated worker and drops the sampling limit,
  so only delivery ever stopped early — and phase B then spawned the worker
  again for the same bytes. Phase A now retains each validated worker response
  in an anonymous temporary file under `--state-dir` and phase B replays it.
  Reuse is an optional accelerator, never correctness-critical state: it is
  bounded to 384 MiB of retained-plus-in-flight bytes and to an artifact-handle
  cap derived from live `RLIMIT_NOFILE` and open-descriptor counts, both
  re-probed at every admission; a 4 GiB-or-half-free filesystem floor is
  reserved for the journal and phase-B staging first; and every artifact is
  verified (physical length, content digest, JSON decode, worker-protocol
  identity) before a single record is published. Anything that cannot be
  measured conservatively declines and reparses — which is why the optimization
  is **Linux-only** today: other platforms have no live descriptor evidence.
  A restart has a frozen plan but no trusted handle, so it parses again; the
  spool is deliberately not journal state. `--json` gains a
  `pdf_extraction_reuse` block so the behaviour is observable rather than
  inferred from timing. Original work by Leonid Bugaev (@buger).

### Changed — autoindex refuses reruns that would strand documents

- **Deleting an indexed file and rerunning `xerj autoindex` now fails instead
  of exiting 0.** Nothing in the pipeline removes the documents, aliases,
  graph edges or catalog entries that the deleted file published, so a rerun
  that ignored the deletion left them live and searchable with no source file
  behind them. The rerun is now refused before any remote call other than the
  endpoint-readiness ping: no mapping, delete-by-query, bulk, refresh, graph
  or catalog write is attempted, and the journal is not appended to. The error
  names the removed files and their content keys — the first ten, then an
  `… and N more` tail, since a whole unmounted subtree can vanish at once —
  and gives three recovery routes: restore the files and rerun, rebuild in
  place by deleting the named indices and the state directory, or rebuild
  under a new state directory, prefix and brain. `--fresh` is refused for the
  same case, because it does not delete those documents either. `--json` emits
  the same facts — every removed entry, uncapped — as
  `xerj.autoindex.unsupported_sync_delta.v1` on stdout with exit 1. Deleting a
  file that was only ever *skipped* is not refused: a junk file publishes no
  documents, and the stale junk-catalog sweep removes its one catalog row, so
  nothing is stranded.
- **Files added after a plan was frozen are now called out on stderr.** They
  are still not indexed by a rerun that resumes an existing plan — the plan is
  a crash-resume boundary, not a folder-sync generation — but the run now
  lists them, records them as skipped (exit 3, completed-with-junk) and points
  at `--fresh`, which rebuilds the plan in place and picks them up. Adding or
  changing files and rerunning keeps working; only removals are refused.
- **`xerj brain` no longer turns an absent or zero node-count probe into an
  automatic reset.** Journal/server disagreement now fails with the journal,
  URL, prefix, and brain identity plus recovery instructions, including the
  explicit `--fresh` rerun for a genuinely wiped data directory. Probe
  transport and malformed-response failures remain errors instead of being
  reported as an empty destination. A nodes index that was deleted out from
  under a surviving brain meta doc reads as absence and reaches that recovery
  text, rather than surfacing as a raw HTTP 404.
- **`--fresh` still recovers a state directory whose journal cannot be
  parsed.** The rerun gate reads the durable plan before the run starts, and a
  plan that is malformed, or recorded for a different root/URL/prefix, is fatal
  to a resume — but not to `--fresh`, which deletes that journal unread. The
  preflight is now no more fatal than the open it precedes: under `--fresh` the
  unreadable plan is reported on stderr and rebuilt from the current folder.
  Without `--fresh` the refusal is unchanged. Because the removal gate has no
  comparison basis in that case, the warning says so: documents already
  published for files that are now gone cannot be identified from an unreadable
  plan and are not deleted.
- **Refusal and skip listings are capped at ten entries plus an "and N more"
  tail.** Unmounting a bind mount under an indexed root vanishes every content
  group at once, so the uncapped listing was one rendered entry per journalled
  file. `--json` still carries every entry.

### Fixed

- **An index whose metadata will not parse is refused instead of silently
  reopened with an empty mapping**
  ([#202](https://github.com/xerj-org/xerj/issues/202)). `Index::open` treated
  "this file is not there" and "this file does not parse" as the same thing:
  `load_schema`/`load_settings` mapped ENOENT, EACCES, EIO and malformed JSON
  alike to one anonymous error, and the caller answered all of them with a
  fresh dynamic mapping. Measured on a field deliberately mapped `keyword`:
  after `schema.json` was truncated to half its bytes the index still opened
  `Ok` with `field_count = 0`, the field came back `None`, and one further
  document re-inferred it as `long` — a mapping silently replaced by a
  different one, with nothing in any response saying so. Absent stays absent
  (indices predating create-time schema persistence legitimately have no
  `schema.json`); present-but-unparseable now fails the open with an error
  naming the file. `es_mapping.json` — the full-fidelity mapping behind
  `GET /_mapping` — was logged-and-ignored on a parse failure and now fails the
  index too, on the boot scan, on snapshot restore and on a retry.

  **Visible on upgrade:** a node whose data dir already holds a corrupt sidecar
  now boots **red** with that index unserved, where it previously came up green
  with an empty mapping. It joins the failed set
  [#206](https://github.com/xerj-org/xerj/issues/206) introduced, so every
  surface that set feeds already reports it — measured on a node holding two
  user indices, `victim` with a truncated `schema.json`:

  ```
  GET  /_cluster/health          -> red, unassigned_primary_shards: 1
  GET  /_cluster/health?level=indices
                                 -> victim red, unassigned_info.details = the
                                    absolute path of schema.json and the parse error
  GET  /_cat/indices             -> "red open victim <uuid> 1 0 0 0 0b 0b 0b"
  GET  /_cluster/indices/failed  -> the reason, plus the retry and delete calls
  PUT  /victim, POST /victim/_doc, POST /victim/_search
                                 -> 503 no_shard_available_action_exception
                                    carrying that same reason
  GET  /health/ready             -> 200 "ready (degraded): 1 of 2 indices
                                    serving, 1 failed to open; see GET
                                    /_cluster/indices/failed"
  POST /healthy/_search          -> 200
  ```

  The readiness counts above are the two-index test router's. A running node
  also holds its internal `.xerj_*` indices, so the same condition on the shipped
  binary reads `ready (degraded): 15 of 16 indices serving, 1 failed to open` —
  the 200 and its meaning are unchanged, only the totals move with what else the
  node holds.

  Recovery is the three doors #206 added. Two were measured end to end on the
  shipped binary: repair the file and
  `POST /_cluster/indices/failed/{index}/_retry` (503 while it is still torn,
  `200 {"reopened": true}` once it is not, health back to green), and
  `DELETE /{index}` (200, directory removed, health back to green). The third —
  restore the index from a snapshot — is the path this change touches (a
  successful restore now clears the recorded failure, which it has to or health
  would stay red after the repair worked); it is covered by an engine-level
  test rather than measured over HTTP. The node stays in kubelet rotation as
  long as any index is still serving.

- **Concurrent sidecar writes can no longer manufacture the torn file the reader
  now refuses.** `write_file_atomic` staged every write in
  `path.with_extension("tmp")` — one shared name for all writers of that file —
  so two concurrent writers of one sidecar (`PUT /{index}/_settings` racing
  another settings update, two API-key mints for `api_keys.json`) both opened it
  `O_TRUNC`, interleaved their bytes, and each renamed it into place. The rename
  is atomic; the content being renamed was not one writer's. Measured with the
  old shared name, 4 threads × two different-length settings bodies × 200
  rounds, over four runs: **86–294 of 800 writes failed with ENOENT** (the loser
  renaming a file the winner had already moved) and, with those errors swallowed
  as the callers did, **1–16 of 200 rounds left an unparseable `settings.json`**.
  The counts are race-dependent and vary with machine load; what does not vary is
  that both are non-zero on every run with the shared name and **0 and 0** on
  every run with a unique staging name. Each write now stages
  in its own sibling file (`<file>.tmp.<pid>.<seq>`) in the target's directory
  and removes its own debris on failure — which for `api_keys.json` is key
  material. `update_settings` also writes `settings.json` through
  `write_file_atomic` rather than a plain `fs::write`; it was the last
  non-atomic sidecar writer left.

- **`index` on the mapping is now honoured instead of silently ignored**
  ([#204](https://github.com/xerj-org/xerj/issues/204)). `"index": false` was
  accepted by `PUT /{index}`, echoed back verbatim by `GET /{index}/_mapping`,
  and then had no effect at all: the field kept a full inverted index and
  `match`, `term`, `match_phrase`, `prefix` and `wildcard` against it all
  returned the document, where Elasticsearch answers 400. It is the sibling of
  the `doc_values` fix in rc.12 and follows its shape.

  The line is drawn where ES draws it. Since 8.1, `"index": false` on a
  keyword / numeric / date / boolean / ip field keeps the doc-values column and
  stays queryable — `MappedFieldType.isSearchable()` is "has postings **or** has
  doc values" — so those queries keep working here, unchanged. Only a field with
  neither (a `text` field, or anything with an explicit `"doc_values": false`)
  is unsearchable, and a query naming one is now rejected with ES's own
  sentence, `Cannot search on field [f] since it is not indexed nor has doc
  values.`, as a 400 `search_phase_execution_exception` /
  `query_shard_exception`. The check lives in one place — `search_inner` — and
  these surfaces were each measured reporting the rejection rather than
  absorbing it, including the three that were swallowing it: `_search`,
  `_count` on all four selectors (single index, `logs-*`, a comma list and the
  cluster-wide `POST /_count`), `_msearch`, `_msearch/template`, `_explain`,
  `_delete_by_query` and `_rank_eval`. `_explain` answers the 400 instead of a
  confident `"matched": false` for a query it never ran; `_msearch` and
  `_msearch/template` render it as a per-response 400 envelope with `type` and
  `root_cause`, leaving the sibling sub-requests in the batch untouched; and
  `_rank_eval` records the refused request under `failures` instead of dropping
  it from `details` *and* `failures`. It still publishes a `metric_score`, and
  that number is still the mean over the requests that actually ran — which is
  ES's behaviour too — but the batch no longer shrinks silently underneath it:
  a two-request batch in which one is refused now answers `details: {good}`,
  `failures: {bad}`, `metric_score: 1.0` (measured), so a relevance gate reading
  a clean score can see what did not contribute to it.

  A multi-index `_count` follows ES's broadcast rule rather than failing
  outright: the failure status is returned only when **no** index answered
  (`RestStatus.status`, semantics only), so `/t/_count` and a `logs-*` whose
  indices all carry the offending mapping both 400 exactly as `_search` does,
  while a mixed selector returns 200 with the partial count and the refusal
  visible as `_shards.failed: 1` plus a `_shards.failures` entry carrying the
  `query_shard_exception`. What it will not do any more is what it did before:
  report `count: 0` with `"successful": 2, "failed": 0`.

  `exists` is unaffected and still returns zero hits rather than an error, which
  is also what ES does.

  The multi-field query types follow ES's own split rather than a blanket rule.
  An explicitly *named* field is rejected like any other named field —
  `multi_match` / `simple_query_string` / `query_string` with
  `"fields": ["note"]`, and `query_string` with `"default_field": "note"` —
  while a *wildcard* spec such as `"fields": ["*"]` is never an error, because
  ES expands a pattern over the searchable fields and silently drops the rest
  (`QueryParserHelper.resolveMappingField`). Before this, the same user intent
  got two different answers: the `simple_query_string` form was rejected (the
  parser lowers it to a `match`) while the `multi_match` form returned the
  document through the unsearchable field.

  Reaching the `query_string` half of that meant fixing a second
  accepted-and-ignored key in the same family: **`query_string`'s `fields`
  array was never read by the parser**, so `{"query":"x","fields":["title"]}`
  searched every field, and — once `index: false` became a real rejection —
  disagreed with `{"default_field":"title"}`, which is the same request written
  the other way. `fields` now selects the targets for a clause that named no
  field of its own (a `dis_max` over one leaf per field when there is more than
  one, `^boost` honoured) — the same job `simple_query_string`'s `fields` list
  was already doing. A clause that does name its own field keeps it, as in ES.

  The field is dropped from the full-text index at flush and merge only where
  the stored-doc fallback is *equivalent* — a non-indexed `text` field. An
  exact-typed field that kept its doc values keeps its whole-value postings,
  because the fallback scan splits on non-alphanumerics and would start matching
  `192.168.0.1` against `192.168.0.2`. So no byte-saving claim is made for this
  change: for those types the footprint the option implies is knowingly forgone
  rather than bought with wrong answers.

  Known gaps, measured and left open rather than claimed shut. **Aggregations
  and sort do not inherit the check**: only the `query` clause is walked, so
  `{"size":0,"aggs":{"a":{"terms":{"field":"note"}}}}` still answers 200 with a
  bucket built from `_source`, and `{"sort":[{"note":"asc"}]}` still answers 200.
  ES rejects those too, but for an unrelated reason and with an unrelated
  sentence — `Fielddata is disabled on [f] in [i]…`, which it raises for *any*
  `text` field whether or not `index: false` was declared — so reusing this
  error there would misreport a wider divergence as this one. **The field-less
  arms of the stored-document scan are schema-free**, walking every `_source`
  key, so a token living only in an unsearchable field can still match when
  **no** field is named: `query_string {"query":"…"}` and `more_like_this` with
  no `fields` both still return the document. (Their *named* forms are
  rejected — `more_like_this` with `fields` lowers to a `bool.should` of
  `match` at parse time and inherits the check.) **A wildcard `fields` spec
  reaches that same field-less arm**: `simple_query_string` / `query_string`
  with `"fields": ["*"]` lowers to the `"*"` placeholder and is answered by the
  schema-free scan, so it can still match through an unsearchable field. It is
  never an *error*, which is ES's rule and deliberate — but it is not "answered
  over the searchable fields" either, and that is a pre-existing property of
  wildcard expansion, identical on `main`. (`multi_match` with a pattern is
  different: it is answered by the FTS projection, which does apply
  `isSearchable()`, and finds nothing in an unsearchable field.) **The opaque
  `query_string` fallback still ignores a multi-field `fields` list**: a query
  string the parser declines to lower keeps the whole string for the FTS path,
  where a one-element `fields` is carried across as `default_field` and a
  longer list has nowhere to go. And **`_validate/query` never opens the
  index** — it answers from the parse alone, so it reports `{"valid": true}`
  for a body `_search` refuses. That too is unchanged from `main`, but it now
  disagrees with `_search`, so it is written down rather than left to be
  discovered. Every one of these gaps is pinned by an assertion in
  `index_false_is_honoured::documented_gaps_are_still_open_and_still_documented`,
  so closing one fails the test that says it is open.

### Added — incremental `--no-graph` corpus reconciliation

- **`xerj autoindex --no-graph` now reconciles a changed folder instead of
  re-deriving it.** A `--no-graph` run commits a durable *corpus generation*: a
  manifest of content groups (stable group identity, content digest and byte
  length, canonical path plus aliases, dataset assignment, validated output
  counts) plus a sealed source snapshot of the prepared records. A later run
  projects the current inventory onto the committed manifest and publishes only
  the difference — additions, content changes, deletions, renames, junk
  transitions and duplicate-alias moves all converge, and a no-op re-run issues
  no data bulk and appends no new generation. A run interrupted mid-generation
  replays from its own sealed snapshot, so the result never depends on whether
  the source tree changed after the crash. Dataset and mapping identity is
  frozen at generation 1: a file that would need a *new* dataset or a mapping
  change is refused before any remote mutation rather than silently widening
  the schema.

  Scope, plainly: only `--no-graph`. Graph-enabled runs are unchanged, and each
  changed generation still copies and prepares the full corpus (O(N) staging) —
  the win is in what gets published and verified, not yet in what gets read.
  `--snapshot-max-gb` (default 64) caps the logical staged payload; it is a
  payload budget, not a disk-space or peak-memory guarantee.

  Original work by Leonid Bugaev (@buger).

- **Junk keeps the contract it always had on the generated path: recorded,
  never fatal.** A file that cannot be read or recognised (`plan.junk_files`)
  and a record that no dataset assignment accepts (`stats.junk`) both used to
  abort a `--no-graph` run outright — the first with
  `plan has no assignment for <file>`, the second with
  `durable preparation of <file> produced N junk records`. A single zero-byte
  file was enough to make a whole folder unindexable. Both are now counted
  instead: the sealed prepared artifact carries its junk-record count, the
  manifest group carries it into the generation, and the catalog's per-file
  `junk` and run-level `junk_records_total` report it instead of a hardcoded
  zero. Such a run exits **3** ("completed with junk"), the same signal the
  legacy path publishes — the generated path previously returned a flat `0`
  and lost it.

- **`--dry-run` is honoured on an already-generated state directory.** It was
  evaluated only after the pending-replay and reconcile branches had already
  published and committed, so on any state directory past generation 1 the flag
  was accepted, silently ignored, and the destination mutated. It is now decided
  before either branch: it prints the plan a real run would act on — the
  projected reconcile plan, or the sealed plan of a pending generation — and
  returns without opening the journal for write, without snapshot GC, and
  without a single bulk request.

### Changed — `autoindex --fresh` is refused on a durable generation

- **`--fresh` no longer silently destroys a committed corpus generation.**
  `--fresh` deletes the resume journal, and the snapshot GC that follows every
  journal open then sees an empty protected set and removes every sealed
  snapshot — so on a generated state directory `--fresh` would discard the
  committed manifest, the pending replay evidence, and the alias/path/
  stale-record knowledge, while cleaning nothing at the destination. It is now
  refused up front, naming the generation that blocks it, and pointing at the
  plain re-run (which reconciles the change) or at an isolated rebuild with a
  new `--state-dir` and `--prefix`.

  **Unchanged for everyone else.** On a legacy (non-generated) journal —
  including every graph-enabled corpus and anything `xerj brain` writes —
  `--fresh` behaves exactly as before: it ignores the journal and restarts,
  which is what `xerj brain`'s documented self-heal for a wiped data directory
  depends on.

### Migration — pre-generation `--no-graph` state directories must be rebuilt

- **A `--no-graph` state directory written before this release cannot be
  adopted as generation zero, and the first `--no-graph` run against it will
  refuse with a followable rebuild command.** This is a deliberate, versioned
  format boundary (`sync::GENERATION_FORMAT_VERSION = 1`), not a validation
  failure: a pre-generation resume plan records no stable group identity, no
  content byte length and no validated per-group output counts, so no amount of
  evidence in it could reconstruct a manifest group. The refusal prints the
  exact `xerj autoindex` argv for an isolated rebuild into a new `--state-dir`
  and `--prefix`; the old target and the shared `autoindex-catalog` are left
  alone and require explicit, validated cleanup once the new target is
  verified. Nothing is migrated in place and no destination data is touched by
  the refusal. Graph-enabled state directories are not affected.

## [1.0.0-rc.14] - 2026-08-10

### Changed

- **One documented core and memory policy, and the worker knobs are live**
  ([#240](https://github.com/xerj-org/xerj/issues/240)). XERJ had no single
  answer to "how much of this machine may I take?", and the scattered answers it
  did have disagreed with each other — and, on Darwin, with the OS.
  `engine.flush_workers`, `engine.merge_workers` and `engine.search_workers`
  were documented but inert; they now drive real pool widths, and a value of `0`
  refuses startup instead of being silently reinterpreted. `--bulk-mb` above 24
  and `--workers 0` are likewise refused rather than clamped, so a request the
  engine will not honour fails loudly. Only hand-written configs are affected:
  the shipped `xerj.default.toml` has no `[engine]` section.

- **One 16 MiB file no longer costs 65 GB of RSS**
  ([#239](https://github.com/xerj-org/xerj/issues/239)). `split_sections` was
  quadratic in both time and memory: a 16 MiB text file with no blank line
  needed **65.6 GB of peak RSS and 16.3 s** to produce 16.8 MB of sections.
  Now linear — the same input takes **8.3 ms and 35 MB**.

- **BREAKING — `server.bind_address` now defaults to `127.0.0.1`, and a
  cleartext node refuses to publish itself to the network**
  ([#228](https://github.com/xerj-org/xerj/issues/228)). The old default was
  `0.0.0.0` while TLS is off by default, so an out-of-the-box node accepted its
  admin API key over plain HTTP on every interface the host had — verified on a
  stock boot, where `curl -H "Authorization: ApiKey …" http://<lan-ip>:9200/…`
  answered `200` and the same URL over `https` had no listener to hand shake
  with. Auth being on did not help: the credential *is* the thing on the wire.

  A fresh node is now reachable from its own host and nowhere else. Exposing it
  is two statements, not zero: set `server.bind_address` (or `--bind` /
  `XERJ_BIND_ADDRESS`), and — while `tls.enabled = false` — also set
  `server.allow_insecure_network_bind = true` (env
  `XERJ_ALLOW_INSECURE_NETWORK_BIND`). Without the second, startup exits
  non-zero before the data directory is created or a first-run admin key is
  minted. `--insecure` does not evade it; it clears `tls.enabled`, which is
  exactly what the check keys on. Same fail-closed shape as
  [#229](https://github.com/xerj-org/xerj/issues/229), whose TLS-on gRPC check
  is untouched and cannot be relaxed by this opt-out.

  **Upgrading:** a deployment that relied on the old default becomes
  unreachable from other hosts until it sets both; one that already wrote
  `bind_address = "0.0.0.0"` without TLS now fails to start with a message
  naming the setting that unblocks it. The shipped Docker image, compose file
  and Helm chart set both, because a container's network namespace is the
  boundary and its published port is where TLS belongs.

  Two documentation claims that were false as written are now true: the CLI and
  security pages both said `--insecure` "refuses to run with a non-loopback
  bind", and it did not — a `--insecure` node on `0.0.0.0` accepted an
  unauthenticated `PUT /leak/_doc/1` from the LAN (`201`). The startup banner
  also prints the bind address on every listener line, so a loopback node and a
  world-facing one no longer look identical.

  A `bind_address` that is not an IP literal is rejected first, ahead of both
  exposure checks, with the fault it actually has. Host names have never been
  resolved — the pre-#228 code parsed `"{bind}:{port}"` as a socket address and
  failed on them too — but the exposure predicates fail closed on anything they
  cannot parse, so without that ordering `bind_address = "localhost"` was
  refused with a message asserting that localhost "is not loopback" and would
  "serve plain HTTP on a network-reachable interface", and pointed at an opt-out
  that fixed nothing: setting it let the boot run on to the bind and fail there
  instead, after the data directory, the `.xerj_*` system indices, the master
  key and a printed first-run `admin.key` already existed.

### Security

- **API key secrets are no longer stored in the clear**
  ([#201](https://github.com/xerj-org/xerj/issues/201)). `api_keys.json` held
  the secret verbatim. 0600 plus atomic rename is right for a secret a *process*
  owns, but a file has more readers than a process: a backup, a snapshot, a
  container layer, a support bundle, a decommissioned disk. Secrets are now
  stored as a salted SHA-256 (`$ssha256$<salt>$<digest>`) and compared in
  constant time, with existing plaintext records migrated on load. The fast hash
  is deliberate: the secret is two v4 UUIDs — 244 bits of CSPRNG output, never
  human-chosen — so offline guessing is out of reach whatever the hash costs,
  while this comparison runs on *every authenticated request*, where an Argon2id
  would add tens of milliseconds per request and hand out a free CPU-exhaustion
  DoS. The same release makes `_security/_authenticate` report the key's real
  identity instead of a hardcoded `superuser`, and gives the audit chain durable
  storage.

- **The node refuses to start when TLS is on but gRPC would answer in cleartext
  off-loopback** ([#229](https://github.com/xerj-org/xerj/issues/229)).
  `tls.enabled = true` encrypted two of the three data-plane listeners; the
  third never participated, because the gRPC server is built without tonic's
  `tls` feature. Nothing surfaced it — gRPC clients connected and worked
  identically either way — so an operator who enabled TLS and bound a network
  interface got no error, no failed handshake and no symptom, while API keys and
  document bodies crossed the network in the clear. Auth was never the gap;
  confidentiality was. Boot now fails closed unless
  `tls.allow_insecure_grpc_h2c` says otherwise.

### Fixed

- **`multi_match` returns the same hit set before and after `_flush`**
  ([#218](https://github.com/xerj-org/xerj/issues/218)). The memtable and
  segment paths disagreed on multi-token semantics, so flushing an index could
  change which documents a query matched — verified live on a one-document
  index where an OR query returned 0 hits before `_flush` and 1 after, with ES
  returning 1 throughout.

- **Dynamic string fields get Elasticsearch's default `.keyword` multi-field**
  ([#209](https://github.com/xerj-org/xerj/issues/209)). Dynamic mapping
  inferred every string as bare `text`, so `GET _mapping` never showed a
  `.keyword` sub-field and `_field_caps` never listed one. Anyone arriving from
  Elasticsearch — or any tool that reads `_field_caps` for field discovery,
  Kibana included — could not see that `category.keyword` existed.

- **`min_score` is honoured at `size:0`, and an external scalar N keeps its
  ghosts** ([#193](https://github.com/xerj-org/xerj/issues/193)). A
  `size:0 + min_score` body never materialised a hit, so the threshold had
  nothing to filter and `hits.total` reported the raw match count — measured at
  **65** where the same query at `size:100` counted **5**. The tie-break
  comparator item from that issue is deliberately not included here; it belongs
  with [#191](https://github.com/xerj-org/xerj/issues/191).

- **Index blocks can be removed, `read_only_allow_delete` means what its name
  says, and a blocked write answers 403 rather than 500.** Three compounding
  defects: `PUT /_settings` wrote only the display-side map while enforcement
  read the per-index `settings.json`, so a block was acknowledged, invisible to
  `GET /_settings`, and — with `_block` registered PUT-only — removable only by
  restarting with a hand-edited file. The one block that fires automatically did
  the inverse of its name, and clients hitting any of it were told the server
  had faulted.

- **`llms.txt` no longer tells agents six things the binary does not do.** The
  "Running an index for a human" section is executed, not just read: an agent
  follows it literally and then reports the result to its user. Corrected: the
  job-size line is not the first line printed; `--dry-run` does not write
  *nothing* (it creates and locks the state dir and appends a journal record);
  a dry run reuses a frozen plan instead of re-costing the job, so it is not a
  floor for the real run; exit `1` is the catch-all for every error, not
  "endpoint unreachable"; `status` needs the same explicit `--state-dir` the run
  used; and `--workers`/`--pdf-workers` bound the client-side extractor, not the
  server (with `--pdf-workers` capped to 1–4).

- **A failed index can now be inspected, deleted and retried without stopping
  the server** ([#206](https://github.com/xerj-org/xerj/issues/206)). An index
  directory that refused to open at boot was recorded in a map that only
  `Engine::health` ever read. Live-reproduced with a corrupt `snapshot.json`:
  it was absent from `_cat/indices` and `_cluster/state`, `DELETE /{index}`
  answered `404 index_not_found` and left the bytes on disk, and the only
  recovery was to stop the server and edit the data directory by hand. A
  failed index is now a real state: listed as `red` in `_cat/indices`, present
  in `_cluster/state` as an `UNASSIGNED` primary with `ALLOCATION_FAILED` and
  the verbatim open error, enumerated with its reason by
  `GET /_cluster/indices/failed`, reopenable via
  `POST /_cluster/indices/failed/{name}/_retry` once the cause is fixed, and
  deletable through the ordinary `DELETE /{index}`. Searches, writes and
  creates against it return `503 no_shard_available_action_exception` carrying
  the reason instead of a `404` that claimed it did not exist, while the
  metadata surfaces — `GET /{index}`, `HEAD /{index}` (the `indices.exists()`
  every ES client calls), `_mapping`, `_settings`, `_cat/indices` — report it
  as an index that exists, the way ES answers those from cluster metadata
  rather than from a shard. Before this, `HEAD` said the name was free and the
  following `PUT` said it was unavailable.

  Two related defects in the same report are fixed with it. **Readiness no
  longer hard-fails on a partly degraded node** — `/health/ready` returned
  `503` for any red status, so one broken index pulled a pod holding 200
  healthy ones out of service permanently; it now reports `200 ready
  (degraded)` while the node can still serve something and `503` only when
  every index it holds failed to open. And **`_cat/health` no longer prints a
  hardcoded `green`** — the one health surface an existing ES dashboard points
  at was the one that could not report a broken node; it and `_cluster/health`
  now count an unopenable index as an unassigned primary, agreeing with
  `/v1/health` and `/v1/cluster/health`.

  Because `red` is now reachable on `_cluster/health` for the first time, the
  two conditions that gate on it are consulted rather than assumed:
  `wait_for_active_shards=all` and `wait_for_status=green|yellow` both answer
  `408` with `timed_out: true` on a red node instead of `200 timed_out: false`.
  `GET /_cluster/health?wait_for_status=green&timeout=30s` is the bootstrap gate
  every docker healthcheck, CI wait loop and Kibana startup uses; a red node
  used to sail straight through it. A green or yellow cluster is unaffected.

- **A `DELETE /{index}` that could not remove the bytes no longer strands the
  index.** The open-index path pulled the handle out of the engine before
  `remove_dir_all`, so a removal that failed (read-only mount, `EACCES`) freed
  the name while the directory survived: no handle, no failed-index entry,
  nothing on `_cat/indices`, and the next `DELETE` answered `404` — the same
  dead end as [#206](https://github.com/xerj-org/xerj/issues/206), reached from
  the other side. The handle is now restored on failure, so the index stays in
  service and addressable and the operator can retry the delete once the cause
  is fixed.

- **Index templates, ingest pipelines, data streams and ILM policies survive a
  restart** ([#203](https://github.com/xerj-org/xerj/issues/203)). Index
  templates, legacy (v1) templates, component templates, ingest pipelines, data
  streams and ILM policies lived only in memory: `PUT /_index_template/logs`
  answered `{"acknowledged": true}` and `GET` answered 404 after the next
  restart, and the next index that should have matched the template was created
  without it — with no error anywhere. All six are now persisted together in
  `<data_dir>/cluster_state.json`, written atomically (tmp → fsync → rename →
  fsync the parent directory) so the write is committed at the request, not in
  a shutdown hook, and restored in `Engine::new` before the listeners come up.
  A write that fails is rolled back in memory and answered with a 500 instead
  of `acknowledged`. Restored pipelines are recompiled, not just re-read —
  storing the definition alone would have left `?pipeline=x` accepted and
  silently inert after a restart.

  Four defects found while verifying it, each reproduced first:

  - Concurrent management writes corrupted each other (all flushes staged
    through one fixed `.tmp` path — 32 parallel template PUTs returned
    `store_exception`). Snapshot-and-write is now serialized.
  - A rollover interrupted between "backing index created" and "generation
    persisted" wedged the data stream forever: every later rollover recomputed
    the same name and got `409 resource_already_exists_exception`. Boot now
    adopts the highest generation actually present on disk, warns, and persists
    the repair once.
  - A `cluster_state.json` that could not be **read** (EACCES after a uid
    change on a container volume, a backup tool's chmod, EIO) came up as empty
    maps, and the next management write renamed a snapshot of those empty maps
    over a file whose bytes were perfectly good. The load failure now latches:
    every management mutation is refused with a 500 naming the file, and the
    file is not touched, until a boot loads it cleanly. The corrupt-parse arm
    refuses too. The later format-compatibility fence above tightens that
    diagnostic boot further: it creates no `cluster_state.corrupt.json` copy.
  - `DELETE /_data_stream/<name>` recorded the removal before destroying the
    backing indices, so a crash in that window stranded `.ds-<name>-00000N`
    directories that no data-stream API could reach — GET and DELETE answered
    404 while `PUT /_data_stream/<name>` answered
    `409 resource_already_exists_exception` permanently. The order is reversed:
    the removal is recorded only once every backing index is gone, and a
    backing index that cannot be deleted now aborts the DELETE with a 500
    (it was previously swallowed and answered `acknowledged`) so the operator
    can retry. Read that 500 as "did not finish", not as "nothing happened":
    a multi-generation stream still loses every backing index that *could* be
    destroyed, the stream itself stays addressable, and the retry after the
    cause is fixed completes the delete.

  **Known gaps, deliberately not closed here.** `.ds-*` indices that no data
  stream claims — left by a data dir written before this change, or by a
  DELETE interrupted by an older build — are **not** cleaned up or adopted
  automatically; boot now names them in a warning and the recovery is
  `DELETE /<backing-index>` per index, because an orphan may hold the only copy
  of real data. And `aliases.json` still has both swallow shapes that were
  fixed here for `cluster_state.json`: an unreadable aliases file is swallowed
  at boot and overwritten by the next alias write, and a *failed* alias write
  is only logged, so an alias can survive on disk after the stream it named was
  deleted with a 200. Both are pre-existing behaviour, not introduced here or
  made worse here, and are tracked separately.

- **`autoindex` no longer leaves immortal catalog entries for skipped files**
  ([#238](https://github.com/xerj-org/xerj/issues/238)). A file added after the
  resume plan was frozen is skipped and reported in the catalog — but that
  report was written from a per-run `Vec` that nothing durable remembered, so
  once the file left the corpus no run could ever remove its `file:{key}`
  document. The catalog is the data map every `map`, `status` and agent query
  reads, and it kept advertising files that were gone. Skipped files are now
  recorded in the durable plan and swept from the catalog when they leave the
  corpus. The sweep is safe to do completely because a skipped file is never
  indexed and never enters the graph corpus — that one catalog document is its
  entire live footprint. Ordering is deliberate: the plan entry is added only
  after the document is written and dropped only after the delete has landed,
  so a failed catalog bulk is retried by the next run instead of stranding the
  document. This does **not** implement add/change/delete reconciliation for
  *indexed* files, which remains open.

- **Repeated `autoindex` scans keep agent-facing map metadata durable.**
  Dataset source bytes, parser-junk counts and coercion-drop notes now derive
  from committed per-file journal records instead of invocation-local worker
  counters, so an unchanged resume does not overwrite them with zero. Run
  timestamps now distinguish invocation start (`started`) from summary
  generation (`summary_generated_at`), and `junk_records_total` on the run
  document is the same durable sum the per-dataset `junk_records` reports
  rather than an invocation-local counter that read `0` after a no-op resume.
  Existing catalogs whose historical `started` field was dynamically mapped
  as text remain usable: the additive mapping upgrade no longer attempts an
  incompatible text-to-date type change. Concretely, this is catalogs written
  by **v1.0.0-rc.4** — the one release that had `autoindex` but not yet
  dynamic ISO-date inference (added in rc.5). Measured against a live engine,
  adding `started` as `date` to such a catalog is refused 400
  `mapper_parsing_exception` (*"field [started] already exists as [text],
  cannot add [date]"*), which aborts the invocation before any document work.
  Catalogs written by rc.5 or later already inferred `started` as `date` and
  were never affected.

  Three consequences worth knowing before you upgrade:

  - **`bytes` is redefined.** It used to be the bytes the latest invocation
    processed, credited to the first dataset a file was assigned to. It is now
    the complete canonical source size, counted once in **every** distinct
    dataset that source is assigned to. Per-dataset values therefore get
    larger, and summing `bytes` across datasets can exceed the corpus size
    when one source feeds several datasets — exactly as one source already
    contributed to several dataset-local file counts. It is a dataset-local
    source footprint, not a partition of physical storage.
  - **Journals written before this release carry no per-dataset coercion
    record**, because the new `dropped_by_dataset` field defaults to empty
    when an older journal is replayed. Byte and junk counts reconstruct fine
    (their journal fields already existed), but a dataset whose files are all
    unchanged loses its `N field values dropped by coercion` note on the first
    resume after upgrading, and only regains it once one of its files is
    reprocessed. Re-run with `--fresh` if the note matters more than the
    rescan.
  - **`junk_records_total` is narrower than the number it replaces**, and one
    class of failure has left it. It used to be an invocation counter that
    also folded in records the *backend* refused per bulk item; it is now
    parser junk only, so `xerj autoindex map`'s header counts exactly what the
    per-dataset `junk_records` values count. No signal is lost in practice: a
    single per-item rejection already aborts the run with
    `autoindex stopped with bulk/backend failures`, before any run document or
    map is written, so the folded-in number was never observable in a map.
    That abort message now also states how many records were refused, which
    the first-five error sample could not convey.

- **The installer is now fail-closed on checksum *verification*, not just on
  checksum *download*.** `landing/get` printed
  `warning: sha256sum/shasum not found — skipping checksum verification` and
  installed anyway — which is not what "refusing to install an unverified
  binary", three lines above it, implies. It now searches `sha256sum`,
  `shasum`, `openssl`, `sha256`, `busybox` and `cksum`, and stops if none is
  present. A machine with no hashing tool at all can still proceed, but only by
  setting `XERJ_INSECURE_SKIP_CHECKSUM=1` — an explicit choice, made by the
  user, not a silent default. The downloaded checksum is also validated as 64
  hex characters before use.

- **`XERJ_LIBC=gnu` makes the glibc Linux builds reachable from the one-line
  installer again.** `landing/get:61` unconditionally overrode the target with
  `unknown-linux-musl` after the `case` arm had set `unknown-linux-gnu`, so the
  script could never request a linux-gnu asset — while the code read as though
  it could. musl remains the deliberate default (static, no glibc-version
  floor, and xerj links jemalloc so musl's allocator is not in the path), and
  the comment now says so; `XERJ_LIBC=gnu` opts into the glibc build.

### Added

- **`autoindex` reports real progress, an honest percent and an ETA — no run is
  silent** ([#241](https://github.com/xerj-org/xerj/issues/241)). Between 47%
  and 64% of a typical run produced no output at all, which is the worst
  possible behaviour for the two audiences that matter: a person watching a
  laptop they cannot use, and an AI agent that has gone quiet on that person's
  behalf. Progress now covers phase, items done and total, rate and ETA, in both
  modes — a terminal gets a live redrawn line, a pipe gets periodic structured
  `xerj-progress` lines an agent can parse, and every run ends with a single
  `xerj-done` summary. The percent is bytes-based so a large file cannot make it
  stall at 99%, the first ETA is withheld for five seconds and labelled `rough`
  until there is enough evidence for it, and the line names the file currently
  being waited on. `--quiet` still produces exactly nothing on either stream,
  and `--progress none` is honoured everywhere — the resource policy's own
  startup notes were moved onto the same surface so they cannot leak into
  `--progress json`'s single parseable stream.

- **`/get` and `/get.ps1` are counted.** `functions/get.js` (a Cloudflare Pages
  Function) serves the installer straight out of `landing/get`, unchanged, and
  records the request. The install path had never had a counter of any kind —
  `curl` runs no JavaScript, so no page beacon could ever observe it, and the
  number of installs was simply unknown. It records timestamp, country,
  User-Agent and a coarse OS guess; it records **no IP address, no cookie and
  no identifier of any kind**, so two installs by one person cannot be
  distinguished from two installs by two people. It is fail-open: if the
  storage binding is missing or the write throws, the installer is still
  served. **No telemetry was added to the xerj binary, and none is planned.**

- **`metrics/release-downloads.jsonl` + a daily GitHub Action.** GitHub reports
  release `download_count` as a running total and keeps no history, so the one
  uninflated adoption number this project has could be read but never trended.
  One committed line per day turns it into a series. `scripts/adoption-snapshot.sh`
  prints the funnel on demand, including which repo-level numbers are
  contaminated and why.

## [1.0.0-rc.13] - 2026-08-08

### Security

- **All 13 open Dependabot advisories against `engine/Cargo.lock` closed**
  (#220). Twelve by lockfile-only version bumps — openssl 0.10.81 /
  openssl-sys 0.9.117 (8 advisories: GHSA-8c75-8mhr-p7r9, GHSA-ghm9-cr32-g9qj,
  GHSA-hppc-g8h3-xhp3, GHSA-pqf5-4pqq-29f5, GHSA-xp3w-r5p5-63rr,
  GHSA-phqj-4mhp-q6mq, GHSA-xv59-967r-8726, GHSA-xmgf-hq76-4vx2),
  rustls-webpki 0.103.13 (GHSA-82j2-j2ch-gfr8), quinn-proto 0.11.16
  (GHSA-4w2j-m93h-cj5j), rand 0.8.7 (GHSA-cq8v-f236-94qc), webauthn-rs 0.5.5
  (GHSA-22w3-693w-x895) — and one by removal: `protobuf` is gone from the
  dependency graph entirely (GHSA-2gh3-rmm4-6rq5; `prometheus` now builds
  without its protobuf feature — XERJ only ever served the Prometheus text
  exposition format, and two new tests pin that format). A reachability
  analysis found none of the 13 exploitable in a default deployment; closed
  anyway.
- npm developer-tooling bumps (#194): basic-ftp 5.3.1, ip-address 10.4.0,
  js-yaml 4.3.1, ws 8.21.3 — transitive from puppeteer, never part of a
  release artifact.

### Changed

- **The Helm chart now defaults to secure mode** (#213). It shipped
  `insecure: true` — TLS and auth off — while the `/get` installer shipped
  auth on. The default is now secure, and a new `NOTES.txt` states at install
  time exactly what insecure mode turns off.

### Documentation

- Roadmap to 1.0.0 GA from a source review against the user-feedback corpus
  (#212).
- Install one-liner moved first on README, xerj.org hero and llms.txt, plus a
  paste-to-your-agent install prompt (#219).

## [1.0.0-rc.12] - 2026-08-07

### Changed

- **`doc_values` on the mapping is now honoured instead of silently ignored.**
  `"doc_values": false` was accepted, echoed back by `GET _mapping`, and had no
  effect — aggregating and sorting on such a field succeeded, where
  Elasticsearch errors. The doc-values sidecar builder was schema-free and filed
  a column for every `_source` string it saw. The default now follows ES: no
  doc-values for `text`, `annotated_text`, `match_only_text`,
  `search_as_you_type`, `semantic_text`, `binary`, `object` and `nested`; an
  explicit `"doc_values": true` on a text field still builds the column and
  still returns whole-value buckets.

  Measured on a 500k-doc corpus (one analyzed `text` field), force-merged: index
  154,413,167 → 99,424,772 bytes (**1.553x smaller**, `index/raw` 0.433x →
  0.279x) and force-merge 139.13 s → 59.69 s (**2.33x faster**, merge re-encodes
  the sidecar and there are 55 MB fewer bytes to build). The `.dv` artifact falls
  95.9%; every other artifact is byte-identical. A `terms` aggregation on a text
  field got **1.34x faster** (3414 → 2547 ms) after losing its column, because
  walking a column of whole document bodies was slower than the brute path it
  falls back to.

  The saving is corpus-dependent: it removes 90-99% of the doc-values sidecar,
  and that sidecar is 2-37% of index size depending on how large text bodies are
  relative to `_source`. On source-code corpora where `_source` dominates it is
  nearer the low end.

### Performance

- **The per-segment FTS reader is cached instead of rebuilt on every query.**
  `FtsIndexReader::open` sat inside the per-segment search loop and performed two
  zstd decompressions per field — the whole `.post` blob and the whole `.meta`
  array — into owned buffers, then discarded them. Measured on one production
  field: ~50 ms and ~41 MB per (segment, field), per query.

  Readers are now cached per (segment, field-set), charged against the segment
  hydration budget under a new `fts_reader` category (visible in
  `_nodes/stats`), and evicted at merge completion. Segments are immutable, so
  no invalidation is needed; if the budget refuses the charge the reader is
  returned uncached, so an oversized corpus degrades to the previous behaviour
  rather than growing without bound.

  Measured p50 on 500k docs with the query cache disabled: `match` on text
  166.29 → 57.85 ms (2.87x), 500-doc page 168.96 → 64.05 ms (2.64x), `prefix`
  170.91 → 69.22 ms (2.47x), `wildcard` 180.52 → 74.79 ms (2.41x), `fuzzy`
  306.16 → 195.25 ms (1.57x), `match_phrase` 495.58 → 339.90 ms (1.46x).
  Brute-scan families (`function_score`, `boosting`) are unchanged — their cost
  is scanning and scoring, not opening a reader.

### Fixed

- **`xerj autoindex` no longer duplicates an index when an extractor improves**
  (#178). Datasets were inferred by comparing each file's whole field-name set,
  including names the extractor invents rather than reads from the file
  (`defs`, `symbols`, `symbol_count`, `title`, `page`, …). A better parser makes
  those names appear, which moved the file into a different dataset — and the
  dataset slug is an ingredient of every document `_id`, so the file was
  re-indexed under a new id in a new index while its previous document stayed
  behind, unreferenced. Measured on a 846-file Rust corpus, re-planning with an
  improved symbol extractor grew the index from 53,873 to 53,902 documents with
  29 stale leftovers and exit code 0; on a 2,328-file C corpus, 7,923 to 8,023.
  Clustering now uses only the field names that came from the file, so gaining
  or losing extractor fields can no longer re-home a document: both re-plans
  hold at 53,873 and 7,923.

  Nothing is deleted, so the first *re-plan* after upgrading (`--fresh`, a new
  `--state-dir`, or a lost journal) re-homes the files whose datasets merge and
  leaves their previous documents in place — 53,873 → 58,855 on that Rust
  corpus, a one-time cost paid once instead of once per extractor change. An
  ordinary re-run reuses the journal's frozen plan and is unaffected. Datasets
  inferred from real schemas are untouched: that corpus went from 9 datasets to
  7 (the two symbol-presence variants of one code dataset merged, likewise two
  section-presence variants of one prose dataset), and the C corpus from 407 to
  405, with every document accounted for in both.

## [1.0.0-rc.11] - 2026-08-04

### Added

- **`xerj autoindex` pins the server's embedding execution identity** (#160).
  Vectors produced by two different embedding backends — or by two different
  models, tokenizers, or vector widths within one backend — are not comparable,
  so mixing them in one index silently degrades every similarity result rather
  than failing. A semantic autoindex run now fetches
  `GET /v1/embedding/identity`, records the returned opaque digest in its
  journal before the index is created, and requires the same digest on every
  resume; a mismatch aborts before the next index-create or `_bulk`. There is
  no new command or setup step — `xerj autoindex ./reports` calls the endpoint
  itself over the already-configured `--url`, and plans with no semantic fields
  skip it entirely.

  The endpoint is authenticated (cluster-read) and deliberately opaque: it
  returns a version, the *effective* backend, a digest, the semantic contract,
  whether that backend is resumable, and a sanitized reason when it is not. It
  never returns credentials, provider URLs, model names, or local paths.
  `dimensions` is reported only for the backends whose width the server
  actually pins (`lexical`, and `onnx-experimental` at 384); it is omitted for
  `neural`, whose width comes from the loaded model's `hidden_size`, and for
  `proxy`, whose width is whatever the remote returns.

  `lexical` and `onnx-experimental` are resumable. `neural` and `proxy` are
  not: neither attests immutable model bytes, so a fresh semantic run is
  allowed but a resume is refused.

- **A detector that connects documents by what they say, not only by what they
  cite** (#164). Every shipped detector was structural — `wikilink`, `mdlink`,
  `href`, `pathcite` and `cratecite` need an explicit citation, `sequence` and
  `samedir` need a filesystem relationship — so a folder of PDFs or saved pages,
  where no document links to another, produced a graph whose only edges ran
  inside single documents and along directory chains. `sharedterm@1` emits a
  `shared_term` edge (weight 0.45, confidence 0.5) between two documents that
  use the same distinctive words, and carries those words as the edge evidence
  so a wrong link is inspectable exactly like a wrong `mdlink`.

  Measured on 240 arXiv PDFs filed in 24 topic folders (236 extractable), same
  binary, same corpus: before, 11,854 edges — 11,642 `sequence` inside single
  documents plus 212 `same_dir`, with 212 cross-document edges and **none**
  crossing a folder. After, 462 `shared_term` edges (271 further candidates
  refused by the density cap), 377 of them joining documents in different
  folders and 296 joining different top-level topics. Two independent runs
  produced identical counts and identical evidence.

  Density is capped rather than trusted: a term in more than 10% of the corpus
  is that corpus's vocabulary and is ignored, a link needs at least two shared
  word families (so a singular and its plural are not two signals), and no
  document takes more than 5 `shared_term` edges. What the cap refused is
  reported by the run as `edges_capped` instead of vanishing. A corpus of ten
  documents or fewer therefore produces no `shared_term` edges at all, which is
  the 10% rule being honest rather than an exception.

  The honest limit, which the docs and the console now state: this links
  documents that share VOCABULARY, not documents that share MEANING. Terms are
  compared as strings with no stemming and no model — XERJ's built-in embedder
  is lexical feature hashing and this detector does not use even that. Against
  the corpus's own topic filing, a random pair of those documents shares a
  top-level topic 17.0% of the time; `shared_term` links do so 35.9% of the
  time. Real signal, and a noisy one.

### Changed

- **BREAKING: an existing semantic `autoindex` state directory cannot be
  resumed** (#160). A journal written before this change carries no pinned
  identity, so there is no way to prove the server still produces the same
  vector space; rather than assume it, the resume fails closed with *"this
  semantic autoindex journal predates embedding identity pinning and cannot be
  resumed safely"*. The same path fires when a previous `--no-semantic` run is
  followed by a semantic run over the same state directory. Recover by
  rebuilding with `--fresh` and a new `--prefix`; before reusing the old
  prefix, delete and recreate its prior autoindex indices. Non-semantic runs
  and brand-new state directories are unaffected.

- `EmbeddingProxy::new` now rejects an endpoint that is not `http(s)` at
  construction time instead of at first request, so an invalid
  `embedding.default_endpoint` falls back to lexical at startup and
  `/v1/embedding/identity` reports `lexical` — the backend the server will
  really use — rather than `proxy`.

## [1.0.0-rc.10] - 2026-08-03

### Security

- **The Console data-sources proxy is no longer a way around a brain** (#149).
  The Console router is merged onto the engine routers after their layers are
  applied, so `authz_middleware` never ran on it and no index-visibility scope
  was installed; its only filter matched `.xerj_` (underscore) and therefore
  missed the reserved `.xerj-memory-` namespace. Any authenticated Console
  session of any role could read another tenant's second-brain documents, and
  the indices/fields listings leaked brain names and mappings. All three
  handlers now refuse the reserved namespace with the same `404` a system index
  gets, and the prefix moved to `xerj-common` so there is one definition.

- **Snapshot and restore can no longer reach another tenant** (#152).
  `authorize_expression` waved every wildcard through before consulting the
  principal's grants. That is sound where the engine expands a pattern over the
  caller's visible set, but `create_snapshot` walks the index map itself and
  `restore_snapshot` expands against the snapshot manifest and then removes and
  rewrites the index directory, never passing the visibility funnel. A
  non-superuser `POST /_snapshot/{repo}/{snap}/_restore {"indices":".xerj-memory-*"}`
  therefore rolled every tenant's brain back to the backup instant. Patterns on
  those two verbs are now decided up front: one that may reach the reserved
  namespace requires an explicit grant, exactly as an index template's
  `index_patterns` already did. Narrower patterns are untouched.
  The create handler also read `indices` only in its array spelling, so the
  string form ES equally accepts fell through to "absent" and captured the whole
  node; both spellings now parse, and patterns resolve against the
  visibility-filtered index list. (This also fixes `{"indices":["*"]}`
  previously producing an empty snapshot.)

- **Scripted updates are bounded by the same script budget as search** (#153).
  `_update` and `_update_by_query` reach Painless through
  `transform_document_serialized`, which established neither the request
  deadline nor the fault sink, so every evaluation fell back to its own full
  slice. With a fresh context per `;`-separated statement, a script's cost
  multiplied by its statement count and again by the hit count, and
  `wait_for_completion=false` detached the whole thing with no ceiling. One
  deadline now governs the request, faults are captured once rather than per
  document, and the hit loop stops at the deadline and reports `timed_out`
  honestly instead of a hardcoded `false`.

### Fixed

- **A client disconnect can no longer brick an index** (#150).
  `index_document` held the collection-publication guard across
  `self.schema.read().await`, the only suspension point between `begin()` and
  `commit()`. The PUT handler runs inline on the connection task, so a
  disconnect dropped that future; parked behind schema evolution's write lock
  the drop landed inside the interval and left the guard `Pending`, setting a
  sticky poison flag. Every later read and write on that index then failed
  permanently, and `_close`/`_open` do not reconstruct the index, so only a
  process restart recovered. The schema guard is now taken before the interval,
  which contains no `await` at all and cannot be cancelled part-way. Storage
  failures that occur before any visibility mutation now cancel the publication
  instead of poisoning it, so a transient disk error fails one request.

- **Columnar aggregations no longer drop a segment's documents** (#151, #143).
  `field_needs_brute_fallback` kept the fast path whenever any flushed segment
  carried a field's doc-values column, but array suppression is decided per
  segment, and the executors skip a column-less segment. Indexing scalar values,
  refreshing, then indexing array values and refreshing produced a terms
  aggregation that silently omitted the second segment's documents; term
  predicates and numeric metrics undercounted the same way. Coverage must now be
  total, and the check moved into `seg_field_kind` so it also covers sub-metric
  fields, `top_hits` sort fields, `matrix_stats` fields and composite sources.

### Thanks

- @Nicolas0315 for four detailed, measured performance reports (#144, #145,
  #146, #147) on fuzzy/wildcard term expansion, top-k scoring and the unused
  skip table, the `bbq_*`/`int8_hnsw` mapping contract, and filtered kNN never
  reaching the HNSW graph. Each arrived with a standalone harness and honest
  caveats. They are tracked for the following release rather than rushed into
  this one.


### Changed — runtime-field types (can break existing mappings)

- **`mappings.runtime` is now validated against ES's runtime-field type
  registry**, not against the mapping-property type list. Runtime fields
  have their own, much smaller registry in ES: `boolean`, `composite`,
  `date`, `double`, `geo_point`, `ip`, `keyword`, `long`, `lookup`.
  Anything else is refused at index creation with the same
  `mapper_parsing_exception` ES emits ("The mapper type [x] declared on
  runtime field [f] does not exist…").

  This cuts both ways, and both directions were wrong before:

  - **Newly rejected.** Types that are perfectly valid *mapping
    properties* but illegal under `runtime` — `text`, `match_only_text`,
    `nested`, `object`, `geo_shape`, `shape`, `point`, `dense_vector`,
    `sparse_vector`, `binary`, `percolator`, `completion`, `histogram`,
    `alias`, `join`, `version`, `constant_keyword`, `wildcard`,
    `date_nanos`, the range types, and the narrow numerics (`integer`,
    `short`, `byte`, `float`, `half_float`, `scaled_float`,
    `unsigned_long`, which collapse into `long`/`double` at runtime) —
    used to be accepted with a `200`. **A mapping that declared any of
    these under `mappings.runtime` will now fail to create.** Change the
    entry to the runtime type ES would have required (usually `keyword`,
    `long` or `double`), or move the field into `properties`.
  - **Newly accepted.** `composite` and `lookup` are runtime-ONLY types
    and are absent from the property list, so they were previously
    rejected outright — even though `lookup` runtime fields are
    implemented on the search path (via search-body `runtime_mappings`)
    and covered by the conformance suite. Both are now accepted under
    `mappings.runtime`.

  Scope: this is `PUT /{index}` **validation** only. It does not change
  evaluation: `mappings.runtime` is still stored and never evaluated —
  runtime fields are only computed when supplied in the search body as
  `runtime_mappings`. Search-body `runtime_mappings` and
  `PUT /{index}/_mapping` still do not validate runtime types at all
  (they never did) — unchanged here.

  Prior to this, the rule was half-enforced: `flat_object`/`flattened`
  were special-cased into a `400` while every other ES-forbidden type
  passed.

### Security

- **`x-forwarded-for` is no longer trusted for client identity** (#76 S5-4).
  The per-IP auth rate limiter and the audit-log source address used to be
  read straight out of `x-forwarded-for` — a header the caller writes. A
  rotating `x-forwarded-for: 1.2.3.$RANDOM` therefore reset the quota on
  every request, so the unauthenticated magic-link redeem and bootstrap-claim
  endpoints could be brute-forced without limit, while honest clients (who
  send no such header) stayed throttled. Client identity now comes from the
  TCP peer: `ConnectInfo<SocketAddr>` is threaded through both the plain and
  the TLS listener, and a handler reads it via the new `ClientIp` extractor.

  Forwarding headers are still honoured behind a real reverse proxy, but only
  from a proxy the operator has declared: the new **`server.trusted_proxies`**
  setting takes IP addresses or CIDR blocks (`["10.0.0.0/8", "::1"]`) and is
  **empty by default — an unconfigured node believes nobody**. When the peer
  is a declared proxy the forwarded chain is read right-to-left (the left end
  is caller-authored) and the right-most address that is not itself a listed
  proxy wins; a malformed element stops the walk rather than being stepped
  over. Malformed entries in the setting fail startup instead of silently
  widening or narrowing trust.

  Operators terminating TLS or load-balancing in front of XERJ must set
  `server.trusted_proxies` to their proxy's address, otherwise every user
  behind it shares one rate-limit bucket.
- **Cluster control frames are now authenticated (issue #75).** The inter-node
  transport on `cluster.port` accepted any TCP connection that spoke its wire
  format: anyone who could reach the port could send Raft `RequestVote` /
  `AppendEntries` frames and steer consensus. Every frame now carries an
  HMAC-SHA256 tag over a per-connection random challenge issued by the
  receiver, the sender's node id, the frame's position in the connection, and
  the payload; tags are compared in constant time, and the tag is verified
  before the payload reaches the JSON deserialiser. The challenge makes a
  captured frame non-replayable, including against a different node.
- **Cluster mode fails closed.** `[cluster] enabled = true` now requires
  `cluster.auth_secret` (or `XERJ_CLUSTER_AUTH_SECRET`), minimum 16
  characters. With cluster mode on and no secret from either source the node
  **refuses to start** instead of running an unauthenticated control port.
  Single-node mode — the default — is unaffected and needs no secret.

### Breaking

- **The cluster wire format changed and is not backward compatible.** A node
  running this version cannot talk to a node running rc.9 or earlier in either
  direction: the receiver now speaks first (magic, version, challenge) where
  the old protocol expected the sender to open with a length-prefixed JSON
  frame. Upgrading a cluster requires stopping every node and restarting them
  on the new version with a shared secret configured — a rolling restart will
  not interoperate. Only cluster mode is affected; it is off by default, so
  single-node deployments need no action.
- Scope note: the HMAC authenticates frames, it does not encrypt them. Cluster
  traffic is still plaintext and the secret is cluster-wide, so it proves
  cluster membership, not per-node identity. `cluster.port` still belongs on a
  trusted network segment; mTLS remains unimplemented.

### Added

- **`script` query support** (#87). A Painless predicate evaluated per document,
  wired into the doc-scan path. Missing fields fail closed the way modern
  Elasticsearch does: `doc['missing'].value` errors rather than coercing to a
  default, because since ES 7.0 reading `.value` off an empty `ScriptDocValues`
  throws (6.x returned a type default with a deprecation warning and 7.0 removed
  that fallback). Coercing would mean a script filter with a typo'd field name
  matched *every* document. The guard idiom ES documents still works, because
  that idiom is `doc['x'].size() == 0 ? <default> : doc['x'].value` and
  `.size()`, `.length` and `.empty` are total on a missing field.

  Compiled scripts are now cached, bounded at 128 entries and 512 KiB of source
  per thread. A script is evaluated once per document, so re-parsing per
  document made the per-document cost scale with the script's *size*. The cache
  key is attacker-supplied source, so the bound is a security property rather
  than an optimisation detail.

- **`getDayOfWeekEnum().getDisplayName()` in Painless** (#92).

### Fixed

- **Painless resource-limit trips surface instead of returning a wrong score**
  (#97, #123). `MAX_CALL_DEPTH = 32` is a real ceiling on legitimate recursion,
  and in the scoring paths it failed *silently*: a script recursing past it did
  not error, it returned a wrong score.

  Two failure classes had been collapsed into one and they need opposite
  treatment. Unsupported or unparseable script syntax still degrades quietly,
  which is what ES does and what seven of nine call sites rely on. A tripped
  resource limit now propagates, because a silently wrong score is undetectable.
  `_search`, `_msearch`, both template variants, scroll, async_search,
  `_rank_eval`, `_explain` and pivot transforms report it; `_count`, `_reindex`,
  `_delete_by_query` and `_update_by_query` refuse rather than act on a selection
  a fail-closed script truncated. All nine `eval_painless` call sites were
  audited and behaviour changed at none of them.

  `_rank_eval` records the fault per request in `failures` rather than failing
  the batch, and `_explain` — a diagnostic endpoint whose `matched` verdict does
  not depend on scoring — answers and discloses the fault rather than refusing.
  `_delete_by_query` and `_update_by_query` previously returned HTTP 200 with a
  400-shaped body, so a client branching on the status saw success.

  `MAX_CALL_DEPTH` stays 32: re-measured in release, the parser's worst case is
  40,720 bytes per call level, so depth 32 already uses 1.20 MiB of a 2 MiB
  worker stack.

- **`/_msearch` no longer skips the request-time script guard** (#111, #124). It
  built its sub-requests straight off `parse_request`, bypassing the validation
  the single-search path applies. Measured: a 240,531-byte `_msearch` body whose
  items carried scripts that `_search` rejects with a 400 came back 200 on every
  item. `_search/template` and `_msearch/template` had the same hole.

  Closing it initially made `_msearch` *stricter* than `_search`, because the
  new guard walked the whole body while `_search` checks six specific fields. A
  single `GuardedField` definition now drives every entry point, with the
  `aggs`/`aggregations` pair resolved exactly as the executor resolves it
  (`aggs` wins when both are present, regardless of document order). A
  compile-time assertion pins the guarded set at six, because removing an entry
  was not a type error and silently reopened the bypass.

- **`<field>.keyword` predicates resolve against the parent column** (#110).
  Four raw lookups inside `resolve_pred` failed closed, asserting that a flushed
  segment held no matching row, while the in-memory arm resolved the same field
  correctly. Measured: a `filters` aggregation on `extension.keyword` reported
  200 documents out of 3,200, and a `term` query on `group.keyword` reported 0
  hits out of 6,000.

- **`fast_aggs` falls back to brute force for unresolvable nested fields**
  (#104). Found live on real Kibana and OpenSearch Dashboards sample dashboards:
  `geo.dest` terms, `machine.ram` avg and the doubly-nested `machine.os.keyword`
  terms all returned empty or null through the fast path while the brute path
  computed the right answer.

- **The columnar histogram paths honour `config.limits.max_buckets`** (#121,
  partial). `exec_date_histogram` and `exec_histogram` carried their own
  hardcoded `65_536`. Measured at cap 37 on a 12,000-document index: 38 buckets
  returned, no error. The brute paths ignored the setting too, so this was live
  in both executors. **Still open:** `exec_terms` has no bucket cap at all, which
  is the actual OOM vector; see #121.

- **`bucket_script` resolves `_count` and evaluates parenthesised ternaries**
  (#95, #105). A `_doc_count`-weighted bucket and the `bucket_script` inside it
  disagreed about how many documents the bucket held. Verified per aggregation
  type. Parenthesised ternaries silently resolved to a null bucket value,
  because the split only looked at paren depth 0. Two equal infinities compared
  *unequal*, because `inf - inf` is NaN and an epsilon-only `==` rejected
  identical values.

- **Named ES date formats are resolved for `ignore_malformed` validation**
  (#89). Named shorthands were matched as literal text rather than expanded, so
  a value genuinely valid under the declared format was silently dropped on
  ingest. Confirmed against a real OpenSearch 2.11.1 node, which accepts the ISO
  strings and rejects the bare numbers — the opposite of what XERJ did.

- **Two `xerj-engine` tests no longer flake under parallel load** (#100). One
  mutated a process-wide bucket cap under a lock that only protected
  participants; the other depended on wall-clock timing. Measured on a 32-core
  box under 14 CPU hogs: 12/12 and 10/10 green, where the prior tree was 18%
  red.

- **`_field_caps` reports `flat_object`/`flattened` rather than generic
  `object`** (#86), and **`flat_object` is treated as an alias for `flattened`**
  for OpenSearch callers.

- **Keyed (object-shaped) bucket aggregations merge across multi-index search**
  (#103), and **`X-OpenSearch-Version` is sent to OpenSearch callers** (#101).

### Performance

- **`parse_date_ms` no longer pays an unbounded scan per document** (#91, #96).
  The no-colon zone-offset branch added in #91 sat before the naive fast paths
  and cost roughly 2x on offsetless ISO values, a hot per-document ingest path.
  The probe now reads a fixed six-byte window at the end of the value and
  nothing else — a constant window is sufficient because `%z` closes the pattern
  and chrono requires the whole value to be consumed.

  Measured across three independently built, CPU-pinned harnesses: offsetless
  ISO 217.0 → 110.2 ns/op, and the long-value classes that an interim scan-based
  guard regressed (4 KB prose 1171.4 → 198.3, 64 KB 16278.0 → 1009.2) are back
  at or below the pre-guard cost. Best-or-tied on every class, with zero
  mismatches across 72,462 constructed values.

## [1.0.0-rc.9] - 2026-08-01

Ninth release candidate: the **cross-platform correctness release**.
Headline for users, stated plainly because it is the most serious defect
this project has shipped: **the Windows binaries published as rc.4
through rc.8 could not start.** Every index creation returned
`Access is denied. (os error 5)`, and because the console bootstrap
creates a system index on first boot, the server aborted before it ever
listened. `xerj.org/get.ps1` installed those binaries for three weeks.
The cause was one unguarded Unix idiom in the durability path; the fix
is four lines, and a Windows runner now boots the binary, writes a
document, reads it back and indexes 400 datasets on every pull request.
Nothing else caught this because, until this release, no CI job had ever
*run* the binary anywhere except Ubuntu — `release.yml` built eight
targets and executed none of them.

The release also closes the self-contained half of the post-audit
security backlog — a snapshot location that could still escape
`data_dir`, index-name validation missing at the create boundary, a
field limit that dynamic ingest enforced and explicit mapping updates
did not, an unserialised magic-link redemption, and node identity leaking
from an unauthenticated endpoint — and it carries the consequences of
taking the Windows lesson seriously: a test pass over the surfaces that
shipped untested, and the four real defects writing those tests exposed.
A create-time file-descriptor exhaustion reachable from an index-create
request, inline `<script>` contents indexed as prose, XML record election
that changed shape between runs of the same file, and a core dump on
`xerj … | head`. The Rust suite grows from 1,420 to 1,486 test
functions; conformance is unchanged at 1360 passed / 0 failed / 3
skipped. The only performance change is a memory regression fix in the
ONNX embedding path; no speed measurement was taken and none is claimed.
The `_reindex` keyset behaviour is untouched — a test around it was
wrong, not the code, and that distinction is spelled out below because
it is easy to misread as a data-loss bug.

### Fixed

- **The server can start on Windows.** `xerj_common::fsio::fsync_dir`
  was `std::fs::File::open(dir)` followed by `sync_all()`, with no
  platform gate. On Windows, obtaining a handle to a *directory* that
  way always fails with `ERROR_ACCESS_DENIED (os error 5)`: the standard
  library cannot pass `FILE_FLAG_BACKUP_SEMANTICS`, which that call
  requires. `IndexStore::save_snapshot` fsyncs the parent directory
  after publishing the segment manifest, so **every index creation
  returned an I/O error**, and the unconditional `xerj-console`
  bootstrap — which creates `.xerj_users` on first boot — turned that
  into a fatal `xerj-console bootstrap` error at startup. Introduced in
  `297be60` (2026-07-12) as part of the power-loss-ordered publish
  chain, so it affected rc.4, rc.5, rc.6, rc.7 and rc.8. Windows now
  takes a separate `fsync_dir` that returns `Ok(())`, because Windows
  exposes no directory-flush primitive at all — `FlushFileBuffers` is
  not defined for directory handles. **State the consequence honestly:
  durability on Windows is weaker than on Unix.** The file contents are
  still fsynced by the callers before the rename; what is not forced is
  the rename itself, which relies on NTFS metadata journalling. That is
  the best available behaviour on the platform, and it is now a
  documented property rather than an error returned on every call.
- **`xerj … | head` no longer aborts with a core dump.** The Rust
  runtime installs `SIG_IGN` for `SIGPIPE` before `main`, so once the
  reader closed the pipe the next write returned `EPIPE`, `println!`
  panicked with `failed printing to stdout: Broken pipe`, and the
  release profile's `panic = "abort"` turned that into a core dump —
  which also swallowed the command's real exit status. The binary now
  restores the default disposition before anything can write to stdout
  (unix only), so the kernel terminates the process on the signal and
  shells report the conventional 141. Found because `xerj autoindex map
  | head -80` core-dumped on every run of a use-case harness. **Two
  consequences worth knowing.** Client sockets are unaffected: the
  standard library writes those with `MSG_NOSIGNAL` (`SO_NOSIGPIPE` on
  macOS), so a client disconnecting mid-response still surfaces as
  `EPIPE` and cannot signal the server. The server's own stdout does
  change — previously tracing discarded write errors and a server
  outlived a vanished stdout; now the next log line terminates it.
  Supervised deployments redirect stdout to a file or `/dev/null` (the
  `brain` boot path included), so in practice this reaches only
  `xerj | reader-that-leaves`.
- **Inline `<script>` and `<style>` contents are no longer indexed as
  prose.** The HTML extractor's `skip_until` state suppressed the
  *tags* inside those elements but the tokenizer's text branch appended
  their contents to the buffer unconditionally, and the closing tag
  continued without discarding it — so the buffered CSS and JavaScript
  were flushed into the document body at the next tag boundary.
  Minified stylesheets and scripts became indexed prose in every HTML
  record: BM25 noise, and — the reason this is more than a quality bug
  — inline configuration blocks carrying API keys, tokens and internal
  endpoints were stored in `_source`. For the "point xerj at my project
  folder" use case that is a real exposure. Script and style elements
  are now treated as the raw-text elements they are — the scanner jumps
  to the literal end tag instead of tokenising inside them, which also
  bounds an unclosed `<script>`: the tail of the file is discarded
  rather than buffered and indexed. Two narrower ways the leak survived
  a first attempt were caught in review and are closed here. A trailing
  `/` inside an *unquoted* attribute value (`<script src=/cdn/lib/>`)
  was read as a self-closing tag, so the element was never entered —
  legal HTML5, since `script` is never a void element. And an end tag
  whose name ran on into a non-alphanumeric character
  (`</script-foo>`) was accepted as the terminator, ending raw text
  early and indexing everything after it. The tag scanner now tracks
  attribute state to decide self-closing, and the end-tag test follows
  the spec's appropriate-end-tag rule: whitespace, `/`, or `>`.
- **XML record election is deterministic.** `elect_record_tag` chose the
  repeating element with `max_by_key` over a `HashMap`, and `max_by_key`
  settles ties by iteration order — which is seeded randomly per map.
  When a wrapper element and one of its children were both structured
  and equally frequent, elections over the *same bytes* chose different
  tags: 200 in-process elections over one fixture split 89 / 111. The
  record count and the locators held, so documents kept their `_id`s,
  but each build gave them a completely different field set, and field
  inference for that dataset never settled across builds. The comparison
  is now total — most occurrences, then the outermost tag, then the
  lowest name — with the outermost tag as the principled key: when a
  wrapper repeats as often as its child, the wrapper is the record and
  the child is one of its fields. **Two limits worth being precise
  about.** This bites across *independent* builds — a `--fresh` run, a
  new state directory, a second machine, a CI reproducibility check —
  not across ordinary repeated runs, because the resume journal skips
  files whose content digest has not changed. For the same reason an
  index built before this fix does **not** heal itself: it keeps
  whatever shape it rolled until the XML file's bytes change or the
  index is rebuilt with `--fresh`.
- **Three further inference elections are deterministic.** Reviewing the
  fix above found the same defect — `max_by_key` over a `HashMap`, ties
  settled by a per-instance random hash seed — at three more decisions,
  each with a wider blast radius than the XML one. Each now has a
  tie-break argued from what the decision means rather than a copied
  rule. The log extractor elects the template an *entire file* is parsed
  with, and every line missing that template is demoted to a
  continuation, so a flip changes the record count; ties now go to the
  most specific template, in the order `parse_kind` already tries them
  (app before clf before syslog), so an ambiguous file resolves the way
  an ambiguous line does. Field inference elects the date encoding
  written into the mapping, the catalog and the coercion plan; ties go
  to the lowest `DateEnc` in declaration order, which is `parse_date_str`'s
  own richest-first priority, so a field mixing `…T00:00:13` with
  `… 00:00:13` names the encoding that concedes the least. The third
  elects a field's entity type, keyword versus text.

- **The ONNX embedding path no longer materialises every window upfront.**
  `embed_semantic_jobs` built and cloned the passage text of every window
  before embedding any of them, so peak memory scaled with the whole job
  rather than with what was in flight. Windows now carry
  `(start, end, passages)` and their texts are built lazily and moved
  into `embed_batch`, with at most two batches in flight. This closes a
  memory regression at default settings (#71); no throughput measurement
  was taken and no speed claim is made.

### Security

Five items from the post-audit backlog, each with a regression test
where the defect is testable, plus one found while writing this
release's tests. The three that remain open are listed under Known
limitations rather than quietly omitted.

- **Snapshot repository locations could still escape `data_dir`
  (#73, High).** The residual half of F-PATH-02: a `settings.location`
  containing `..` that resolved to a *nonexistent* target slipped past
  the guard, because canonicalisation fell back to a lexical path and
  the containment check compared components of a path that had never
  been resolved. `..` components are now rejected outright, before any
  resolution. The regression test covers both the direct form and
  laundering it through an explicit allowlist entry.
- **Index-name validation is enforced at the create-index boundary
  (#80).** `IndexName::validate` existed and was applied elsewhere;
  the ES-compatible `create_index` handler did not call it. Wiring it up
  is defense in depth against path traversal through index names — the
  accepted set is unchanged, so no previously valid name is now
  rejected.
- **The field limit applies to explicit mapping updates (#76 S5-5).**
  `max_fields_per_index` was enforced on dynamic ingest but not on the
  explicit `add_fields` path, so `PUT _mapping` and schema evolution
  could push an index past the ceiling that exists to bound mapping
  explosion.
- **Concurrent magic-link redemptions cannot both mint a session
  (#76 S5-3).** The check-then-consume sequence in the redeem handler
  was not serialised, so two requests carrying the same token could both
  pass the used-check. Redemption is now serialised behind a gate.
  Stated precisely, because it is easy to overclaim: in the shipped
  engine the second session was already being blocked incidentally, by
  the index's optimistic concurrency rejecting the colliding write — so
  the observable pre-fix exposure was a 500 with internal detail instead
  of a clean 401, plus a single-use guarantee that rested on what the
  store happened to do with racing writes rather than on the handler.
  The fix moves that guarantee into the handler where it belongs. This
  is covered by 25 rounds of 6 concurrent redemptions.
- **The unauthenticated `cluster/info` body no longer leaks the node
  identifier or exact build version (#76 AUTHZ-2).** It now carries only
  mode and uptime.
- **A per-index setting could exhaust file descriptors at index-create
  time.** `index.xerj_ingest_shards` — added in rc.8 to *reduce*
  descriptor usage — was accepted with only a `>= 1` check, and the
  ES-compatible create-index handler threads the request body's
  `settings` through verbatim. `IndexStore::open` eagerly creates a
  directory and opens a WAL file descriptor per shard, so
  `{"settings":{"index":{"xerj_ingest_shards": 100000}}}` was a
  single-request descriptor and inode exhaustion — precisely the failure
  the setting exists to prevent. The override is now bounded to
  `1..=256`, the same ceiling `EngineConfig::validate` already enforced
  on the global `engine.ingest_shards`. Out-of-range values are
  **refused, not clamped**, so a typo falls back to the engine default
  rather than silently taking a different one. Found while writing the
  tests that rc.8 shipped without.

### Added

- **CI runs the binary on macOS and Windows.** A new
  `autoindex-fd-smoke` job on an `[ubuntu, macos, windows]` matrix boots
  the server under a constrained descriptor budget (soft 256 / hard
  4096 — between the pre-fix need of roughly 6,400 for 400 datasets and
  the post-fix 600), asserts a create-index / index-document / search
  round-trip, autoindexes a 400-dataset corpus and fails on any
  `EMFILE`. macOS runners default to `ulimit -n 256`, the exact
  condition the rc.8 descriptor bug regressed under, so the job
  reproduces it rather than approximating it. The explicit round-trip
  exists so that the next platform-specific break names the operation
  that failed instead of surfacing as an opaque "server exited during
  boot".
- **The use-case harnesses run in CI.** A new `usecase-smoke.sh` gate
  drives the second-brain transcript (overview counts and detector
  families, ego evidence and `not_shown`, idempotent link, unlink
  retiring rather than deleting, `as_of` replay, and the normative
  `hops > 2` / self-edge 400s and unknown-brain 404), the MCP tool
  surface (initialize handshake, the four `xerj_brain_*` tools listed
  with their honesty strings, and a real `tools/call` round-trip), and
  autoindex discovery with post-discovery search. These harnesses
  already existed but were manual, so a regression in the brain API or
  the MCP tool list was caught by nothing. Wiring them up immediately
  found two assertions that could never have passed: the transcript
  pinned detector versions at `@1` when five of them are at `@2`, and
  the autoindex idempotency check compared whole `_cat/indices` rows
  including on-disk size, which legitimately changes across a re-extract.
- **THE MAP's bounded-graph claims are tested.** The knowledge-graph
  pipeline shipped in rc.7 on two load-bearing claims — at most 13
  groups at any scale, and byte-identical output across runs — with no
  automated test in any language behind either; the only exercise was a
  manual browser script needing a live server and a real Chrome. The
  pipeline is pure dependency-free JavaScript, so 23 offline cases now
  run in under a second over four corpus scales (8, 12, 40 and 120
  folders, straddling the 12-cluster cap): the group bound with at most
  one pooled "everything else" body, every file landing in exactly one
  group with the membership lists and the index agreeing both ways, link
  conservation against the honesty row the UI computes from,
  run-to-run determinism, the pairwise bundle bound, and `as_of` replay
  making a retired link live again.
- **Regression cover for four surfaces that shipped without any.** The
  DOCX decompression and paragraph caps from rc.7 (both verified by
  removing each guard in turn and confirming the matching test fails);
  the six autoindex extractors that had no direct tests at all — HTML,
  CSV, JSON, JSONL, XML and SQLite, 49 cases; the magic-link redemption
  race hardened in rc.9's Phase-2 work, driven with 25 rounds of 6
  concurrent redemptions; and the per-index WAL shard override,
  including a reopen test that pins a shard count, writes documents that
  live only in the memtable and WAL, restarts, and asserts both the
  layout and every document survive. An installer lint gate covers
  `landing/get` and `landing/get.ps1`, which had no verification of any
  kind despite being the only user-facing entry point.

### Changed

- **A test was measuring the CI runner's core count, not the code.**
  `reindex_pages_past_10k_via_keyset` failed on any machine with real
  core count (`left: 0, right: 10050`) and passed on CI's two-core
  runners, so `cargo test --workspace` was red for every developer and
  green for the project. Turbo ingest routes documents to memtable
  shards by worker thread: on two cores they all land in one
  incidentally-visible shard, on a real box they scatter into
  unpublished shards that the search the reindex pages over cannot see.
  The sanity assertion above it passed either way because
  `live_doc_count` reads the version map rather than the search surface.
  The test now refreshes the source. **The `_reindex` product path was
  never affected** — driven end to end over HTTP, bulk-indexing 10,050
  documents and reindexing with `size: 1000` reports
  `total: 10050, created: 10050, batches: 11` and the destination counts
  10,050, both with and without an explicit refresh. This was the test
  reaching under the API, not a reindex defect, and it is recorded here
  because a bare changelog line about "reindex returning 0 documents"
  would badly misrepresent it.

### Known limitations

- **DOCX truncation at the decompression cap is silent.** When a
  document's XML inflates past the 72 MiB cap the reader simply hits
  end-of-file; because a truncated tag reads as a clean `Eof` rather
  than an error, nothing increments the junk counter. A bomb-truncated
  document is therefore indistinguishable from a legitimately short one
  — the caller gets a well-formed record that is quietly missing
  content. The memory bound itself holds and is tested; only the
  *reporting* is missing. Making truncation observable would change the
  statistics autoindex reports, so it is deliberately left for a
  separate change.
- **`index.xerj_ingest_shards` is only read in its nested spelling.**
  `{"index": {"xerj_ingest_shards": 1}}` is honoured;
  `{"index.xerj_ingest_shards": 1}` is not, unlike the neighbouring
  `index.sort.*` settings which accept both. Nothing in the product
  emits the flat form, so this is an inconsistency rather than a break.
- **A brain is still not an authorization boundary.** Any authenticated
  caller can read any brain's `overview` and `ego`; with a shared engine
  that is a multi-tenant exposure. This needs an access model rather
  than a patch and is tracked in #79, deliberately not rushed into a
  release candidate. The concrete part of that issue — the walker
  indexing `.env` and other dotfiles — was fixed in rc.7.
- **Cluster transport control messages are still unauthenticated**
  (#75). Reachable only with cluster mode enabled, which is off by
  default.
- **`x-forwarded-for` is still trusted for client identity** (#76 S5-4),
  which means a caller can spoof the address the per-IP rate limiter
  keys on. The fix needs `ConnectInfo` threaded through the shared
  serve and TLS path and is deliberately not bundled with the smaller
  items above. *(Fixed after this release — see Unreleased.)*
- **Windows durability is weaker than Unix durability**, as described
  under Fixed. The platform provides no way to make it otherwise.

## [1.0.0-rc.8] - 2026-07-31

Eighth release candidate: an **autoindex robustness + code-AST release**.
Headline for users: `xerj autoindex` no longer crashes with `Too many open
files` on large source trees, and source code is now parsed into searchable
symbols instead of plain text.

### Added

- **AST code extraction.** Source files are parsed with tree-sitter — Python,
  JavaScript, TypeScript, TSX, Rust, Go, Java, C, C++, Ruby, PHP, C#, Bash —
  rather than indexed as prose. Each file carries its `language`, a structured
  `symbols` array (`{name, kind, line}`), and a searchable `defs` list, so a
  query like `class Model` retrieves the file that defines it. Grammars are
  version-decoupled via `tree-sitter-language`, so one tree-sitter 0.25 core
  serves all of them and adding a language is a grammar dep plus one row.

### Fixed

- **Autoindex `Too many open files` (os error 24) crash** on large repos. Each
  index holds segment mmaps, a WAL and a merge task, so discovering a repo that
  infers hundreds of datasets (WooCommerce ≈ 413 indices, ~16 descriptors each)
  exhausted file descriptors on a default macOS soft limit (256), failing near
  the start. At startup the server now raises `RLIMIT_NOFILE` toward the hard
  limit using a **concrete** `rlim_max`: macOS rejects `setrlimit(RLIMIT_NOFILE)`
  when `rlim_max` is `RLIM_INFINITY` (its launchd default), so passing that back
  was a silent no-op and the limit stayed at 256 — the raise now sets a concrete
  ceiling and steps down for the `kern.maxfilesperproc` cap (best-effort).
  Reproduced and fixed against real django, redis and WooCommerce clones.
- **jemalloc `background_thread currently supports pthread only`** printed on
  every macOS launch — the `background_thread` option is now Linux-only, where
  it is supported; the decay policy is unchanged.
- **Empty console dashboards on a fresh or brain-only engine.** The AI, RAG,
  vector, agent-memory and logs dashboards fell back to mock data when their
  backing index was empty; each dashboard's nav entry is now gated on its data
  existing, so a brain-only launch opens on Second Brain and an empty engine
  shows only System.

## [1.0.0-rc.7] - 2026-07-30

Seventh release candidate: the **second brain and security-hardening
release**. Headline for users: point one binary at a folder with
`xerj brain <folder>` and get a bi-temporal, evidence-carrying link graph
over your own documents, with a console view — THE MAP — that stays
readable at any corpus size. Alongside it, six network-reachable security
findings from a whitebox self-audit are fixed.

### Added

- **`xerj brain <folder>`** — one command turns a folder into a queryable
  knowledge graph. Structural detectors (wiki-links, markdown links,
  hrefs, folder adjacency, reading order) record, for every link, the
  exact sentence that created it and the byte offset it came from.
  Measured: 10 notes → 25 links in 0.1s; a 943 MB repo copy → 7,571 files,
  2,128,360 notes, 20,930 links in 322.7s.
- **Graph HTTP API** — `POST`/`DELETE /_graph/{brain}/link`,
  `GET /_graph/{brain}/ego`, `GET /_graph/{brain}/overview`. Bi-temporal:
  links are retired, never deleted, and any past moment is replayable via
  `as_of`. Traversal is capped at 2 hops per call by design — XERJ is a
  search engine with a graph-shaped index, not a graph database.
- **THE MAP** (console) — a Canvas knowledge-graph view whose default
  "helicopter" altitude is bounded by construction: any corpus collapses
  to at most a dozen named groups plus one catch-all. Measured 19,299
  links / 1,550 documents → 13 groups in 44 ms; a synthetic 50,000-link
  budget → 13 groups / 60 bundles in 122 ms, byte-identical across runs;
  60 fps pan/zoom/expand. Small corpora skip clustering and draw real
  notes, so the view is adaptive rather than one-size.
- **Cross-file-type links** — new `pathcite@1` and `cratecite@1`
  detectors relate documents to the code and files they cite, so a PDF or
  a design note can be shown next to the crate it references, with the
  citing text as evidence.
- **Agent surface** — four MCP tools (`xerj_brain_ego`, `xerj_brain_link`,
  `xerj_brain_unlink`, `xerj_brain_overview`) under
  `contract: xerj-second-brain/1`.
- `limits.snapshot_repo_allowlist` — an Elasticsearch `path.repo`
  equivalent bounding where snapshot repositories may live.

### Fixed

- **Security (unauthenticated):** `POST /_sql` with a deeply nested
  `WHERE` clause overflowed the stack and aborted the whole process. The
  WHERE parser now bounds recursion depth (`MAX_SQL_DEPTH`), as the
  query_string parser already did.
- **Security:** search-template parameters were spliced into the query
  JSON unescaped and re-parsed, so a parameter could inject query
  structure and drop the template's own filter. Parameters are now
  JSON-escaped and can no longer emit structural tokens.
- **Security:** a snapshot repository's `settings.location` was an
  unvalidated absolute path, so snapshots could read and write index data
  outside `data_dir`. Locations are now bounded by `data_dir` or the new
  allowlist.
- **Security:** the console's session `last_seen` refresh blind-wrote a
  stale copy of the session, which could resurrect a session revoked
  moments earlier. The refresh now re-reads and skips revoked sessions.
- Console: the Second Brain dashboard is no longer always present — it
  appears once at least one brain exists, so a user who has never run
  `xerj brain` is not shown an empty dashboard.
- Console: several second-brain counts reported the engine's 10,000-hit
  floor as an exact total on multi-million-note brains; totals are now
  exact, and floors are shown as `≥`.

### Notes

- The second brain's link detection is **structural and lexical** — there
  is no LLM and no semantic model in the loop, and the node-store embedder
  remains a lexical feature hash. Scale beyond the measured corpus above
  is not claimed.

## [1.0.0-rc.6] - 2026-07-28

Sixth release candidate: the **semantic-analytics and bounded-memory
release**. Headline for users: a `knn`/`semantic` query can now carry
`aggs` in the same `_search` request — aggregations run over the
retrieved top-k neighbour set, forced onto the exact path so bucket
counts are exact — and a semantic hit can opt into a `_passage` field
that returns the winning chunk's text with byte-exact provenance.
Underneath, the seven immutable-segment hydration caches now share one
process-wide, cgroup-aware memory budget, and the storage layer gains a
bounded, cancellable selective-hydration primitive (deliberately not
wired to any engine route yet). Two correctness holes are closed: kNN
could return a superseded vector when a document ID was re-inserted
while owning the HNSW entry point (fix arrived from the probelabs fork),
and an oversized Painless expression could exhaust the native stack
instead of returning a bounded error. One change arrived from outside
the org: transparent gzip request-body decompression, contributed by
Vinz2168, which unblocks Filebeat and the other ES clients that compress
by default. ES-YAML conformance holds at 1360 passed / 0 failed / 3
skipped. Each change states its own limits in place; the aggregation
gaps left open are listed under Known limitations.

### Added

- **kNN + aggregations in a single request.** `POST /{index}/_search`
  with a `knn`/`semantic` query plus `aggs` now returns aggregations;
  previously the `aggs` field on kNN responses was hardcoded `null`. No
  new API surface — the existing `aggs` request field is honored on the
  kNN path, across all three executors (HNSW, exact brute-force,
  multi-kNN). Semantics follow ES top-level-knn: aggregations run over
  the retrieved top-k neighbour set after the `num_candidates` fan-out,
  independent of `from`/`size` paging — `"size": 0` returns aggs only
  with empty hits, and `hits.total.value` reports the neighbour pool
  (`k`), not a match count, so bucket counts scale with `k`, never with
  index size. To keep those counts exact, an aggs-bearing kNN request
  always executes the exact brute-force path even when it would
  otherwise qualify for HNSW — ANN recall is below 100% and approximate
  bucket counts would be silently wrong — trading ANN speed for
  exactness (no performance claims are made for this path). This
  removes the previous two-step workaround (kNN → collect ids →
  separate aggregation request). Limitation, stated up front:
  `significant_terms` over a kNN slice returns empty — the vector path
  does not yet supply a background corpus; use `terms` for raw in-slice
  counts, or the two-step pattern when statistical significance is
  needed (filed as a follow-up).
- **Winning-passage provenance (`_passage`).** Requesting
  `"fields": ["_passage"]` on a single `semantic` or kNN query adds
  `fields._passage[0]` to each hit: the winning chunk's source `field`,
  zero-based `ordinal`, UTF-8 byte `start_offset`/`end_offset`, the
  `text` slice, and `page` (only when the source carries a numeric
  `page`; omitted otherwise). The text is reconstructed by slicing the
  authoritative `_source` at compact ingest-time offset metadata rather
  than storing the chunk a second time — the committed measurement over
  256 varied Unicode documents reports 80.26 B/doc of raw metadata,
  3.98 B/doc after ZBS2 compression. All three ES `fields` wire shapes
  (scalar, string array, object form) are accepted and normalized
  identically, and scroll continuations keep the passage. Multi-kNN and
  hybrid-fusion queries requesting `_passage` are a deliberate HTTP 400
  with an actionable message — summed or fused contributions do not
  define one winning passage, and rejecting is more honest than
  guessing. Scope: provenance covers the exact per-chunk max-sim
  scoring path; the HNSW-served and SQ8-quantized paths reconstruct a
  passage only for single-chunk values, and a kNN clause on an
  arbitrary `dense_vector` field has no passage metadata, so `_passage`
  may be absent rather than erroring. The internal metadata (reserved
  prefix `__xerj_passage_meta__`) is engine-owned: supplying it at
  ingest is rejected per-document, and it is stripped from `_source`,
  Painless, field discovery, full-text indexing, and aggregations. The
  native `_search` endpoint accepts `_passage` (returned as a hit-level
  key) and rejects any other `fields` entry. The default embedder
  remains lexical feature hashing — neural semantics still require
  opting into neural embedding mode or an embedding endpoint.
- **Process-wide segment hydration cache budget.** The seven
  immutable-segment hydration caches (stored slices, doc values, parsed
  stored values, sort shadows, id positions, row sequences, decoded
  stored bytes) — previously independent or unbounded — now share one
  cgroup-aware retained-payload budget with CAS admission and
  last-reader refunds. Configure with
  `[limits] max_segment_hydration_cache_mb` (default `0` = automatic:
  20% of the effective cgroup/system memory limit, no floor and no
  fixed cap; an explicit value is clamped to 50% with a startup
  warning) or the `XERJ_SEGMENT_HYDRATION_CACHE_MB` env override
  (`auto`, `off`, or a MiB value; `off` refuses every admission — a
  diagnostic switch). Refusal is invisible to queries: the request
  keeps the value it already built and returns the same result,
  uncached; publish-time warm-only values are simply dropped, and a
  counter increments. `GET /_nodes/stats` gains
  `indices.segment_hydration_cache` with limit/current/peak, refusal
  and accounting-error counters, the budget source, and per-category
  breakdowns. Automatic sizing now honors the *tightest* cgroup-v2
  `memory.max` from leaf to root (previously the nearest finite value
  won, overstating the ceiling when a parent limit was smaller). A
  release-build accounting underflow that left counters unchanged is
  also fixed — both counters now clamp atomically and record exactly
  one accounting error. Honesty note, repeated in the docs: this is a
  retained-payload/key ceiling computed from deliberately conservative
  estimates, **not** an RSS bound — query-owned materialization, decode
  scratch, mmaps, memtables, vectors, and allocator behavior stay
  outside it, and a single highly compressed segment can still create a
  transient decode peak before admission (bounded/streaming decode is a
  named follow-up). The motivating profiles (~206 MiB of publish-warming
  RSS on one 4,096-document diagnostic run; 226.35 MiB of cumulative
  live allocations under a merge warm) are attributed measurements, not
  a claimed end-to-end memory reduction.
- **Bounded selective stored-field hydration (storage primitive).** The
  `xerj-storage` crate can now hydrate selected rows and selected
  top-level `_source` fields from a ZBS2 V2 stored section without
  materializing the whole canonical JSON array, with cancellable entry
  points (deterministic checkpoints every 128 rows), a retained-size
  preflight computed from framing metadata alone (fails closed on
  historical zstd frames that omit a content size), and
  malformed-input hardening — checked arithmetic, fallible reserves,
  and framing/varint/bit-width validation keep hostile headers on the
  error path instead of aborting in the allocator. The on-disk stored
  format is unchanged and historical segments remain fully readable.
  This is deliberately **not** wired to any engine route or cache in
  this release — no endpoint, no query change, and no end-to-end
  speedup is claimed. The committed report measures the trade-off on
  its deterministic 20,000-row fixture: one-row hydration at roughly
  59× lower peak RSS (~335 MB full decode vs ~5.6–5.7 MB selective) but
  roughly 4× slower wall time, because RAW/zstd JSON columns still
  stream through all rows — a memory primitive with a CPU cost,
  measured as a single-host storage microbenchmark, and LZ4 columns
  still decompress whole before selective materialization.
- **gzip request-body decompression (external contribution).** Both
  HTTP routers now transparently decompress `Content-Encoding: gzip`
  request bodies before parsing; previously the raw gzip bytes reached
  handlers and failed JSON parsing (the commit reports
  `PUT /_ilm/policy` returning 400 and `POST /_bulk` returning 500).
  This unblocks the ES clients that compress by default — the
  reproduced failure was Filebeat 8.13.4 with default config 400'ing on
  its first ILM setup call, marking the output connection permanently
  failed, and retrying forever with zero documents ingested; after the
  fix the same default run completes and every test event lands and is
  queryable. Semantics note: the decompression layer sits outside the
  body-size limit, so the configured request-body limit now bounds the
  *decompressed* size, not the on-wire size. Only gzip is handled
  (deflate/br/zstd remain unsupported), response compression is
  unchanged, and verification is the PR's own curl + Filebeat runs — no
  automated test was added in the diff. Contributed by Vincenzo
  Lombardo (`Vinz2168`).

### Fixed

- **kNN under document-ID reuse (HNSW entry point).** Re-inserting a
  vector under the external ID that owned the HNSW graph entry point
  could return the old, superseded vector as a hit for that ID, and the
  v2 graph writer could persist a CRC-valid file whose entry header
  paired the current slot's external ID with the orphaned slot's higher
  layer. Both are fixed (arrived from the probelabs fork, PR #64,
  landed via #66): a slot counts as live only when the reverse ID map
  resolves its external ID back to that exact slot, the entry point is
  re-selected as the highest-layer live slot, search excludes
  superseded slots from results (they stay traversable for graph
  connectivity — excluded, not reclaimed), and the writer derives the
  entry header from live topology, repairing a stale in-memory header
  on save. This is a write/search-time fix, not a retroactive file
  repair: graph files written before the fix still load as-is and are
  corrected the next time the fixed writer saves them. The on-disk
  graph format and byte layout are unchanged, and no authoritative
  documents or latest vectors are discarded.
- **Painless: oversized expressions return a bounded 400 instead of
  aborting the process.** Flat shapes the parser can build without
  recursing (`1+1+…` binary chains, `.member` chains, `[index]` chains)
  bypassed the parser's recursion guards, and the evaluator's depth
  guard counted frames only after entering evaluation — so a 5,001-node
  chain could exhaust the native stack before the intended error fired
  (reproduced as a process abort on the default test-thread stack).
  Expression depth is now computed exactly at parse time — 500 accepted
  and evaluated, 501 rejected, covered for all three shapes — and flat
  binary and member/index chains evaluate iteratively. Scripts found
  under `script`/`*_script` keys in `_search` bodies that exceed the
  limit get an up-front HTTP 400: "script evaluation exceeded maximum
  depth; split the expression into smaller statements". The supported
  depth contract itself is unchanged (still 500) — this enforces the
  existing contract before evaluator recursion instead of during it —
  and semantics of accepted expressions (precedence, associativity,
  short-circuiting and error order, `doc`/`params`/`Math` dispatch)
  are test-covered as unchanged. Statement nesting no longer consumes
  the expression-depth budget; it stays bounded by the parser's
  separate limit.

### Changed — internal (no behavior change claimed)

- **HNSW read path behind a storage-view seam.** Search, construction
  traversal, diverse-neighbor selection, diagnostics, and graph
  serialization in the vector index now run through a private read-only
  storage trait with static dispatch, so a future immutable graph
  backend can implement the same read contract without rewriting the
  search path or adding runtime branching. Public API, mutation model,
  distance behavior (cosine/L2/dot), filtered-search and tombstone
  semantics, and graph format v2 are all unchanged — a checked-in
  pre-seam format fixture loads, rewrites, and reopens correctly, and
  parity tests compare exact result ordering and exact serialized
  bytes. The commit's controlled A/B (one 10K × 64-d graph, 30K
  deterministic queries, nine alternating CPU-pinned runs) reports
  −0.31% median qps with identical checksums, stated to be within run
  noise: explicitly not a speedup, and this refactor does not by itself
  reduce ingest memory.

### Docs

- **calltree.ai case study**
  (`docs/case-studies/calltree-analytics/`): three runnable scripts +
  README showing one `POST /conversations/_search` combining
  `query.knn` with `terms`/`percentiles`/`avg` aggregations for
  deep-research questions, over a 130-conversation generated corpus,
  with a primitives table mapping sub-questions to query shapes.
- **RAG hybrid retrieval example**
  (`docs/examples/rag-hybrid-retrieval/`): pure-cosine vs BM25 vs
  `query.hybrid` (RRF) over an 18-chunk synthetic corpus with real
  768-dim EmbeddingGemma vectors, documenting the pgvector-to-XERJ
  drop-in migration (same chunks and embeddings, swap the cosine
  `SELECT` for the hybrid `_search`). The honest result is included:
  on the identifier query, equal-weight RRF ranks the right chunk #3,
  not #1, and the README recommends weight tuning or a query router.
- **daily.dev Postgres CDC case study**
  (`docs/case-studies/daily-dev-postgres-cdc/`): logical-replication
  sync from a pgvector-equipped `post` table into XERJ via `_bulk`,
  with both a polled `test_decoding` consumer and a push-streaming
  consumer with LSN checkpointing (live-verified resuming from
  `confirmed_flush_lsn` after a stop/change/restart). A correction is
  recorded in place: the first draft's "no fused RRF node" limitation
  was the author's own request-syntax error, not an engine gap —
  `query.hybrid` with `fusion: {type: rrf}` is the real syntax.
- **Semantic-analytics use case** on xerj.org
  (`landing/use-cases/semantic-analytics.html`, wired into the
  use-case subnav, hub, solutions index, sitemap, and
  `llms.txt`/`llms-full.txt`): documents the rc.6 kNN+aggregations
  behavior, including every gap listed under Known limitations below.
  All corpora in this Docs group are small and synthetic, and nothing
  in them was timed at any corpus size; daily.dev and calltree.ai are
  the parties whose questions motivated the case studies, not
  customers.
- **Production-deployment recipe** gains "Bound retained segment
  hydration" (the budget knob, env override, clamping, refusal
  semantics, and the stats fields); the passage-retrieval recipe gains
  the `fields: ["_passage"]` example and response shape.

### Known limitations

Known-open items in this release's own features, stated here rather
than left implied:

- **`significant_terms` over a kNN slice returns empty buckets** — the
  vector path does not yet supply a background corpus. Use `terms` for
  raw in-slice counts, or the two-step kNN → ids → aggregation pattern
  when statistical significance is needed. Filed as a follow-up.
- **Aggregations on a kNN query are bounded by `k`:** buckets count the
  retrieved neighbour set only, never the full matching corpus, and
  `hits.total.value` echoes the neighbour-pool size, not a match count
  — the documented top-level-knn semantics.
- **An aggs-bearing kNN request always runs the exact brute-force
  scan:** cost scales with corpus size × dims, `num_candidates` has no
  effect there, and HNSW never serves it. A hybrid (RRF) query carrying
  `aggs` is rejected with a 400, and a kNN clause inside `bool.filter`
  is not peeled and returns no buckets — slice aggregation works for
  pure kNN only.
- **The hydration budget is not an RSS bound** — it caps conservatively
  estimated retained payload and keys only, and a single highly
  compressed segment can still create a transient decode peak before
  admission; bounded/streaming decode is a separate follow-up.
- **The selective-hydration primitive is not wired** into any engine
  read path or the shared cache budget yet, and LZ4 columns still
  require whole-column decompression before selective materialization.
- **Request decompression handles gzip only** (deflate/br/zstd request
  bodies remain unsupported); response compression is unchanged.
- **HNSW graph files written before the entry-header fix** still load
  with the inconsistent header retained; a file is only corrected the
  next time the fixed writer saves it.

## [1.0.0-rc.5] - 2026-07-27

Fifth release candidate: the **real-client compatibility release**. Much of
it came from pointing real Elasticsearch and OpenSearch tooling at
XERJ — Kibana 8.13, Kibana OSS 7.10.2, OpenSearch Dashboards 2.11.1/3.6,
and their own shipped sample datasets (flights, eCommerce, logs) — and
fixing the wrong answers, 500s, and stalls those clients produced. The rest
is storage, ingest and `autoindex` work that did not come from client
testing. Headline for users: the query classes that silently matched
**zero** documents (booleans, `.keyword` multi-fields, `match_phrase` on
arrays — and keyword arrays before a flush, see the qualification below)
now return what ES returns; a real Kibana/OSD instance boots,
logs in, and saves objects against XERJ end to end; and a dashboard firing
several panel queries at once no longer stalls the node. ES-YAML
conformance holds at 1360 passed / 0 failed / 3 skipped. Zero-hit defects
found during this cycle that are **not** fixed here are listed under Known
limitations.

### Fixed — query semantics (classes that silently matched zero docs)

- **Keyword arrays (partial — memtable half only):** a `term`/`terms` query
  on a multi-valued keyword field only ever compared element `[0]` — the
  memtable stored just the first value and the segment keyword column is
  one ordinal per doc — so exact lookups returned silent false negatives
  while `terms` aggregations and `match` (which join all elements) looked
  correct. The memtable path is fixed: those doc-values lookups now bail
  for array fields and fall through to the array-aware source scan, so a
  `term` on a multi-valued keyword field is correct **before** a flush.
  Flushed segments are **not** fixed — the segment keyword column is still
  one ordinal per document, so after a flush a `term`/`terms` on a
  non-first array element can still silently miss. The complete fix needs
  multi-valued segment keyword columns (a storage-format change); the
  regression test
  (`xerj-engine` `test_term_matches_non_first_array_element`) is committed
  but `#[ignore]`d until that lands.
- **Booleans:** `term`/`match_phrase` on a boolean field undercounted to 0
  while a `terms` aggregation on the *same* field bucketed `true`/`false`
  correctly. The trigger was the count shortcut over memtable-resident
  data — i.e. any bulk import before an explicit `_flush`, which is what
  every real importer (OSD's own sample-data loader included) produces.
- **`.keyword` multi-fields:** the brute-force scan's field resolver had no
  multi-field fallback, so `term`/`match_phrase` on `category.keyword`,
  `manufacturer.keyword` and friends matched nothing — `_source` never
  contains a literal `"category.keyword"` key. It now strips the trailing
  segment and retries against the parent when the parent is a leaf value
  (guarded, so a genuinely absent nested/object field is still absent).
- **`match_phrase` / `match_phrase_prefix` on arrays:** both arms only
  handled scalar values, so an array-valued field never matched any
  phrase. ES semantics restored: the doc matches if any element does.
- **`match_phrase` with a non-string query value:** `{"query": true}` was a
  hard parse error surfaced as `search_phase_execution_exception` — the
  exact shape OSD's filter bar sends for a boolean filter pill, which
  broke every panel on the dashboard at once. Scalars are now coerced the
  way `match` already coerced them.
- **Empty query strings:** `match`/`match_phrase`/`match_phrase_prefix`
  with `""` returned 400; ES treats an empty analyzed query as zero terms.
  They now resolve to `match_none` (200, no hits) — this is what Kibana's
  saved-objects `_find?search=*` builds, so that endpoint works again.
- **`geo_point`:** aggregations and `geo_distance`/`geo_bounding_box`
  rejected string-encoded `{"lat": "50.03", "lon": "8.57"}` coordinates
  (the shape the flights sample data ships), and `geo_centroid`/
  `geo_bounds` used a flat lookup that never found a nested geo field.
  Both now behave like ES.
- **Aggregation scripts:** `terms` required a top-level `field` and
  returned `{"buckets": []}` whenever it was absent — any script-based
  terms aggregation silently produced no buckets. `script` is now a real
  key source alongside `field`.
- **Painless date accessors:** `doc['t'].value` returned a plain string, so
  `.getHour()`, `.getDayOfWeek()`, `.getYear()` and the rest of the common
  accessor set failed the whole script. They now parse and extract the
  requested UTC component.
- **Dynamic date mapping:** ISO-8601 strings are inferred as `date` instead
  of `text`; `date_detection: false` was accepted-but-ignored and is now
  honored; a non-date value written into an inferred date field returns
  `mapper_parsing_exception` instead of being silently stored and then
  being invisible to range queries, sorts, and time filters; and
  `date_detection` survives the `PUT /_mapping` merge path.
- **Segment-path parity:** projected FTS bool queries dropped
  `minimum_should_match` (so `match_bool_prefix` with `mm=3` matched every
  single-term hit after flush) and doc-values wildcards did not case-fold;
  nested kNN returned zero hits for any parent doc living in a segment
  because the reassembled `{_id, _seq_no, _source}` shape was not
  unwrapped. `_count` now resolves `terms` lookups.

### Fixed — API surface (what real clients call)

- **Aliases:** single-index endpoints (`GET /{alias}`, `/_mapping`,
  `/_settings`, …) never resolved an alias to its backing index, and
  aliases were never persisted at all — index data survived a restart but
  the alias pointing at it did not. Aliases now resolve everywhere ES
  accepts them and are written to `aliases.json` (atomic temp+rename).
  Together this unsticks a fresh OSD container looping on "Another
  OpenSearch Dashboards instance appears to be migrating the index".
- **`_field_caps`:** never listed declared multi-fields (so `fields=*`
  omitted every `.keyword` entry and index-pattern refresh reported
  "field not found"), and only special-cased the literal `*`/`_all`
  wildcards — any real glob such as `wiki-test*` returned empty caps.
  Both fixed; wildcard/comma index specs also now resolve on 9 further
  endpoints that previously 404'd, 400'd, or — for `_refresh` and
  `_cat/count` — silently reported success / zero documents while doing
  nothing (`_refresh` answered `{"_shards":{"successful":1,…}}` without
  refreshing; `_cat/count` returned 0, indistinguishable from a genuinely
  empty index).
- **`POST /{index}/_update`** never returned a `get` block, so every Kibana
  saved-object write crashed client-side reading `body.get._source`. It is
  now returned exactly when the caller passes `_source`/`_source_includes`/
  `_source_excludes`, matching ES (absent otherwise, so `_doc` responses
  are byte-identical to before).
- **`_source` vs `stored_fields`:** the implicit `_source` suppression that
  `stored_fields` triggers is now only a default — an explicit top-level
  `_source` in the same request wins, as in ES. This is what starved
  almost every column in OSD's Discover.
- **`_bulk`** reports each item's real `_seq_no` and `_version` instead of
  a wall-clock microsecond timestamp and a hardcoded `1`.
- **`_update_by_query` / `_delete_by_query`** honor
  `wait_for_completion=false` and return the async `{"task": …}` form;
  `GET /_tasks/{id}` reports completion with the final response.
- **`/_xpack`** respects `--compat-version` (it had its own hardcoded
  `8.13.0`, which Kibana OSS 7.10.2 surfaced as an opaque "license not
  available" refusal to start), and `/_xpack` + `/_xpack/usage` report the
  real auth state instead of `security.enabled: true` — so `--insecure`
  no longer makes Kibana render a login screen for auth that is off.
- **Missing HTTP verbs:** `_refresh`, `_analyze`, `_msearch` accept GET and
  `_clone`/`_shrink`/`_split` accept PUT, as ES does — the PUT `_clone`
  405 was a fatal error during Kibana's saved-object migration.
- **Login-path endpoints:** HTTP Basic auth (Kibana's interactive realm),
  `POST /_security/profile/_activate`, `GET /_security/profile/{uid}`,
  `GET`/`POST /_security/user/_has_privileges`, and a real
  `GET`/`PUT`/`DELETE /_security/privilege` store with ES-shaped 404s —
  each of these was a 500 or a hang on the Kibana login and home pages.
- **`GET /_cat/templates/{pattern}`** (a 404 that crashed OSD) and
  **`POST /_index_template/_simulate`** (body-only template preview) added.

### Changed — index resolution (can break existing callers)

These three align index addressing with ES 8 and turn some
previously-succeeding requests into errors. Review any caller that relies
on the old behavior.

- **A comma-separated index spec now 404s when a concrete name is
  missing**, implementing ES's default `ignore_unavailable=false`. The
  missing name used to be dropped silently — `POST /real,typo/_refresh`
  answered `200` with `_shards.total: 1`. Wildcard/`_all` specs that match
  nothing are still a valid empty result (ES's `allow_no_indices` default).
- **Wildcard and `_all` expansion no longer sweeps in hidden
  (dot-prefixed) indices**, so an operation like `POST /*/_close` can no
  longer hit `.xerj_users`-class system indices. A dot-prefixed pattern
  opts back in, as ES's hidden addressing does.
- **`POST /_close` refuses a wildcard or `_all` target outright** with ES
  8's `action.destructive_requires_name` `illegal_argument_exception`.

### Added

- **OpenSearch client auto-sensing.** XERJ answers the identity endpoints
  (`GET /`, `GET /_nodes`) per request based on the caller's User-Agent, so
  one running instance serves an `opensearch-py`/`opensearch-js`/OSD client
  and an Elasticsearch client simultaneously, each seeing a
  version/distribution block its own compatibility gate accepts. Explicit
  `--compat-distribution` / `--compat-version` (and the matching env vars)
  still pin the identity for every client. The `x-elastic-product` header
  is no longer sent to a detected OpenSearch caller.
- **Opt-in ingest memory attribution (developer feature).** Setting
  `XERJ_INGEST_MEMORY_TRACE=summary` emits a bounded `xerj.ingest_memory.v1`
  NDJSON ledger — per-owner logical bytes across HTTP bodies, raw and
  parsed semantic sources, prepared docs and vectors, active memtables and
  drained flush snapshots — alongside jemalloc allocated/active/resident,
  RSS, CPU time, and accounting/dropped-event counters, plus a separate
  `/proc`-derived `xerj.process_sample.v1` stream. Default is fully off.
  Merge and read-cache owners are reported `unavailable`, not zero. This
  makes ingest retention *measurable*; it is not itself a memory bound.
  A deterministic bounded diagnostic suite
  (`demo/usecases/autoindex/scale/bounded/`) drives ingest → refresh →
  flush → force-merge → restart with exact count and sentinel checks.
- **Private pprof debugging toolkit.** A Linux-only `debug-profiling`
  feature compiles in bounded CPU and jemalloc heap profiling with no
  network endpoint; artifacts are written mode-0600 to an operator-created
  directory, with `capture.py`/`inspect.py` wrappers that hash the binary
  and every artifact. Not in any shipped feature set; no runtime overhead
  or speedup is claimed.
- **Experimental ONNX embedding backend**, wired through `autoindex` behind
  the `onnx-experimental` feature. Its lazy session initialization is
  cancellation-safe: model loading moved to one process-owned thread whose
  single terminal result is shared, so a cancelled first request can no
  longer strand every later one.
- The ES-YAML conformance runner now exits non-zero when any case fails
  (it previously reported failures but exited 0).

### Changed — performance (concurrent dashboard bursts)

- **Full-corpus aggregations no longer deep-clone the memtable under the
  shard lock.** The `need_full_corpus` path (any request with `aggs` that
  the columnar fast path can't serve) cloned every buffered document's JSON
  tree while holding the per-shard read guard, serialising concurrent
  panels behind O(docs) work each. It now Arc-shares out under the lock and
  clones after releasing it. Measured: 15 concurrent `date_histogram`
  queries against a real ~14k-doc memtable-resident index, 1.2–2.7 s →
  ~150–170 ms (commit `6a54cf5`).
- **Full-corpus aggregations no longer re-decode segments per query.** The
  same path did an unconditional open + decompress + full `serde_json`
  parse of a segment's entire stored section on every request once data had
  flushed. Segments are immutable, so it now uses the existing
  single-flight stored-value cache. Measured: 20 concurrent requests
  against a real flushed eCommerce segment, 15–26 s → 1–4 ms including the
  cold first hit (commit `f6daf70`). A segment read failure during
  full-corpus assembly is now a hard error rather than a silent skip that
  undercounted every bucket.
- **The scan path no longer stalls on cold, concurrent bursts.** Two
  compounding costs: every scanned doc's `_source` was deep-cloned to
  splice `_id` in even though only a deeply-nested `ids` clause needs it
  (now computed once per scan from the query shape), and the raw-fallback
  decode had no single-flight protection, so N concurrent requests against
  a cold segment each paid the full open+decompress. Measured against real
  Kibana OSS 7.10.2 and eCommerce sample data: a 24-concurrent
  `query_string`+`match_phrase` burst immediately after restart (worst-case
  cold caches) went from ~4.5 s per request to 100–165 ms, with the warm
  repeat at 1–5 ms (commit `7e963ff`).
- **Semantic and vector work is bounded by the request.** One absolute
  deadline computed at search entry is carried through single-flight
  waiting, admission, embedding, hybrid/multi-kNN recursion, and exact
  scanning; partials set `timed_out` and `hits.total.relation: gte`, and
  the ES handler aborts its child task when the request is dropped.
  Previously a cancelled semantic search could drain for minutes past its
  timeout. Cold vector segment loads also moved to bounded `spawn_blocking`
  producers so they can no longer pin every async worker.

### Fixed — storage, durability & ingest

- **Raw ingest validates before it publishes.** Caller bytes were appended
  to the WAL before being proven to be complete JSON, so malformed input
  could reserve sequence numbers and become durable, with the later parse
  path silently substituting `{}`. Whole-batch UTF-8/JSON/nesting
  validation now completes before any sequence, WAL, version-map, memtable,
  or schema mutation, and malformed bulk sources return per-item
  `document_parsing_exception`. The parsed `Value` is now the single
  authority for turbo ingest (live indexing and WAL replay could previously
  disagree), and `copy_to` is applied before WAL publication so GET,
  search, flush, replay, and restart all observe the same source.
- **Per-shard WAL buffers bounded to 64 KiB** (was 8 MiB). Every index
  eagerly opens one writer per ingest shard, so a 16×16 default reserved
  ~2 GiB of allocator capacity before a single document was written;
  capacity above a frame cannot batch across requests because every
  acknowledged append already drains the buffer. Measured on 256 empty
  writers: jemalloc allocated 2,147,503,496 → 16,797,064 bytes (−99.2%),
  with ingest throughput ratios of 0.976 (single shard) and 1.020
  (eight shards) and exact replay preserved (commit `761b915`). Cost:
  records larger than the 64 KiB buffer issue more write syscalls
  (measured 1,372 → 3,372 on 131 KiB × 1000), with throughput unchanged.
- **Same-ID writes are serialized.** A keyed publication coordinator now
  spans the current-state/CAS check through WAL, version map, FTS, and HNSW
  publication for single-document paths, closing races that could admit two
  creates, lose an update patch, or let a delete overtake a write. Scripted
  `_update` and `_update_by_query` run through the same boundary, so two
  concurrent `ctx._source.n += 1` requests can no longer both read `n=0`.
  Distinct IDs do not contend. (Turbo batch paths remain follow-up work.)
- **Semantic vectors stay out of full-text indexes.** The dynamic-field
  walkers treated pooled embeddings and their `_chunks` companions as
  ordinary text and fed every float into FTS as a decimal token. In one
  failed diagnostic run that produced 2.338 GiB of vector FSTs inside a
  3.449 GiB partial index. Embedding outputs are now excluded by schema
  designation (not a name heuristic) across pre-analysis, flush, and merge.
  Existing vector FST sidecars are not rewritten on upgrade — reindexing is
  the safe way to reclaim them.
- **The admin API key persists across restarts.** The server wrote
  `data_dir/admin.key` but never read it back, minting a fresh key on every
  start and locking out every client configured against the previous one.
  An existing well-formed key is now reused; anything missing or malformed
  still falls through to fresh generation.

### Fixed — autoindex

- **Content deduplication with crash-safe replacement.** Byte-identical
  paths were indexed independently (one reproduction journaled 2,442
  records for 1,221 live documents). Sources are now hashed with streaming
  XXH3-128 and digest peers byte-verified before one canonical path is
  chosen; every current name is preserved in `ax_paths`. Publication became
  an explicit durable generation transaction (synced `file_replace_start`
  → staged, source-verified extraction → synced `file_done` commit), and
  journal replay repairs torn tails and rolls back failed appends.
  Ambiguous legacy prefix collisions fail closed before any backend
  mutation instead of guessing.
- **Follow-on fixes to that rework:** resume now guarantees exclusive
  plan-key ownership (two files resolving to one key each ran the
  replacement transaction and deleted the other's documents);
  `delete_by_query` repeats until a pass deletes nothing, because the
  server implements it as a single `size:10000` pass and >10k-doc
  generations left permanent ghost documents; and both recoverable error
  paths now name the offending files or journal offset instead of
  advising a blanket `--fresh` re-extract.
- **PDFs are parsed by `pdf_oxide` in isolated same-binary workers.** The
  previous byte scanner interpreted font character codes without resolving
  page resources or ToUnicode maps, producing NUL-separated and shifted
  text that schema inference (correctly) refused to elect as
  `semantic_text`. Workers get process groups, a 1536 MiB `RLIMIT_AS`, and
  descendant kill/reap on timeout — documented as resource isolation, not
  a security sandbox, with no OS memory cap on non-Unix. New
  `--pdf-workers` and `--pdf-timeout-secs` flags. Cost: the no-default
  release binary grows 34,160,328 → 39,018,184 bytes (+4.6 MiB).
- **Text files are classified by sentence density, not line length.** The
  old `avg_len > 60` rule split markdown across two datasets with two
  different field names — adding `## headings` to a document made it *more*
  likely to be treated as a record stream, which split BM25 statistics
  across two corpora. Terminal-punctuation density separates prose
  (0.43–0.57 on the measured corpus) from logs and source (0.00–0.20);
  CSV rows fall on the record-stream side of the same threshold.

### Changed — autoindex (what it produces)

- **Line-oriented text is chunked into overlapping windows** (40 lines, 10
  overlap) instead of one document per line, and prose sections dropped
  from 32 KB to 2 KB with overlap, because BM25 scores per document and a
  single line rarely contains the caller's own wording. Measured on this
  repository (234 files, 170k LOC + docs + 460 commit messages) with 8
  "where/why is X" questions: 3/8 → 7/8 answered, 162,883 → 5,508 records,
  indexing 234 s → 1.9 s (commit `4510189`). Every chunk carries
  `start_line`/`end_line`.
- **`semantic_text` election is by language, not length.** The rule was
  "largest `text` field with `avg_len >= 200`", which elected 300-character
  base64 and concatenated-id columns while skipping a genuinely semantic
  150-character summary. A field must now actually look like natural
  language — `word_ratio >= 0.55` (tokens matching `[A-Za-z]{3,}`) **and**
  `mean_tokens >= 3`. Measured `word_ratio` is 0.00 for
  trace_id/user_id/order_id/numeric columns and 0.78–1.00 for prose, log
  messages and source. This changes which field gets embedded, so it
  affects both semantic-search quality and ingest cost (the built-in neural
  backend measures ~2.8 docs/s). The election note records the numbers that
  drove the decision.

### Added — autoindex

- **`--bulk-timeout-secs`** (default 300, range 1–3600) applies only to
  `POST /_bulk`, so a legitimately slow neural bulk can be accommodated
  without loosening the deadline on control requests. Retries are bounded
  at six attempts with the identical body and deterministic document IDs.

### Docs

- **WordPress core security audit case study**
  (`docs/case-studies/wordpress-security-audit/`): a reproducible record of
  an AI agent auditing real WordPress core (1,492 PHP files, ~619k lines)
  with XERJ as the retrieval substrate — sink census, interprocedural taint
  analysis, authorization graph, POP-gadget hunt — plus a copyable Claude
  Code skill and a step-by-step playbook. The honest result is that **core
  came back hardened**: the documented findings are three Medium-severity,
  known-class items (an incomplete SSRF deny-list missing `169.254.0.0/16`,
  an ImageMagick parse surface, and an unguarded `role` sink in
  `user-new.php`), a fourth downgraded to not-reachable-in-core, and
  verified negatives for IDOR, SQL de-escaping, and sanitizer composition.
  None of it is claimed as a novel 0-day. The published grep-wins
  counter-examples are shown alongside the wins, and the XERJ bug the audit
  surfaced (the keyword-array `term` defect partially fixed above — the
  memtable half) is disclosed.
  Site use case 06 (`/use-cases/code-security-audit.html`) is built from it.
- **Token-usage guidebook** (`docs/TOKEN_USAGE.md`): a measured decomposition
  of what a XERJ answer costs an agent in tokens — envelope overhead,
  answer, and materialized intermediate data — instead of hand-waving.
- **AST + graph + FTS vulnerability research**
  (`docs/research/ast-graph-vuln-detection.md`, `docs/examples/ast-vuln-graph/`),
  including a multi-language tree-sitter taint scanner tested at real
  WordPress scale.
- Verified embedding examples: XERJ with Google AI (EmbeddingGemma, Gemini
  API, ADK) and with any OpenAI-compatible `/v1/embeddings` endpoint.

### Known limitations

Known-open items found while validating this release and **not** fixed in
it. The first three are zero-hit defects — a query returns no documents
that ES would return:

- **`multi_match` looks inverted.** On a 6,022-document index a long query
  returned 0 hits with `operator: "or"` (the ES default) and 2 hits with
  `operator: "and"`. OR must be a superset of AND.
- **`match` against a `semantic_text` field returns 0 hits** for a long
  natural-language string, while the same query against a plain `text`
  field returns thousands.
- **`term`/`terms` on a multi-valued keyword field can still miss after a
  flush** — only the memtable half of that fix landed (see the first bullet
  of "Fixed — query semantics").
- **`autoindex` dataset clustering merges same-shape files** with different
  subjects: 213 source files sharing the schema `{text}` collapse to one
  index, and embedding centroids separate the subject groups only weakly.
- 3 of 1,363 ES-YAML conformance cases are skipped.

## [1.0.0-rc.4] - 2026-07-22

Fourth release candidate: the **production-hardening release**. A 9-review
release-readiness audit produced 17 blockers and ~60 follow-on items; all
were fixed across four hardening waves and verified against the live binary
(ES-YAML conformance 1360/1363, full-matrix benchmark 52 WIN / 0 LOSE / 25
TIE vs live ES 8.13.4). Headline for users: acknowledged writes survive
crashes, wrong-but-200 responses are gone, the node defends itself under
resource pressure, and the bundled Console gains Kibana-quality editable
dashboards.

### Fixed — durability (acknowledged writes survive failure)

- **Acked-write loss closed:** verified WAL prune + power-loss-ordered
  publish chain; `wal_sync="sync"` honored on ALL bulk paths and the
  `wal_batch_ms` fsync loop actually implemented; torn-frame recovery so a
  disk-full/crash tear cannot poison a WAL generation; consecutive `_bulk`
  delete actions no longer dropped; acked deletes survive restart
  (WAL-shard pinning); delete tombstones end WAL pinning segment-durably.
- **Merge-window reads:** GET never 404s during the merge-publish window;
  merges can never silently drop docs; `_forcemerge` is synchronous and
  quiescent like ES.
- **Startup/data safety:** exclusive `node.lock` on the data dir (second
  process fails fast); data-dir format marker refuses newer-than-supported
  or corrupt dirs BEFORE any destructive GC; refuse-on-corrupt snapshot
  restore; HNSW persistence fsyncs file + dir around rename; periodic
  background flusher no longer aborted at spawn; sharded-WAL FTS replay
  restored on reopen.

### Fixed — correctness (no silent wrong answers)

- **Fail-loud sweep:** the silent-wrong-query classes on `_search` are
  rejected with real 400s (unknown fields, unsupported constructs), as are
  CCR auto-follow (501), remote reindex, `has_child`/`has_parent`, learned
  fusion, and SQL `HAVING` — previously all silently returned wrong data.
- **Doc CRUD wire semantics:** real per-doc `_version` and ES seq_no
  convention; `POST /{index}/_doc/{id}` route added; malformed bulk docs
  rejected per-item with ES-shaped 400s instead of stored as empty `{}`.
- **Aggregations:** real `sum_other_doc_count`; composite bucket keys typed
  from the source field mapping; `multi_terms` raises `too_many_buckets`
  as a real 400 past the cap; `top_hits` emits the doc's real `_seq_no`.
- **Query semantics:** ES-exact date resolution for range bounds (rounding,
  format, date math); Painless compares strings as strings (every string
  previously compared equal) with depth + source-length guards; highlight
  offsets correct on multibyte text; `combined_fields` OR pooling;
  `query_string` fallback discloses operator handling; kNN threads
  filter+boost through top-level kNN and honors similarity cutoffs.
- **Doc-values counting (P0):** a `range` filter on non-numeric values
  admitted every memtable document in `size:0`/`_count`/filter-agg paths
  (a one-day date window over-counted 3.4×); date/keyword range bounds now
  compile to the columnar fast path instead of falling to the brute scan.
- **Multi-valued fields:** a field that is multi-valued anywhere in a
  segment no longer ships a lying doc-values column that silently dropped
  those docs from count shortcuts — consumers fall back to the exact scan.

### Added — resource governance (the node defends itself)

- Parent circuit breaker keyed on ACTUAL RSS, global search pool, disk
  flood-stage watermark, per-query memory guard, ANN coverage guard, and a
  search timeout that actually preempts term-dictionary walks; scroll and
  async-search contexts are TTL-swept and capped. Classic node-killers
  (huge `size`, deep pagination windows, bucket explosions) return bounded
  400/429 instead of taking the process down.

### Added — security

- gRPC listener authenticated; health probes exempt from auth; constant-time
  compare for the admin API key; `admin.key` and TLS private keys created
  0600; CORS configurable and restrictive by default; API keys persist
  across restart with an honest role surface; `/_memory` list paginated
  with a documented auth model.

### Added — Console: Kibana-quality editable dashboards

- Durable backend CRUD for dashboards (create/replace/patch/delete with
  ETag optimistic concurrency) — user dashboards survive localStorage
  clears AND server restarts; a real panel builder with live preview
  (11 viz types, index/query/metric pickers); free-form `{x,y,w,h}` panel
  resize + move; first-launch seeding of 13 built-in dashboards as durable
  managed rows; edit-mode chrome no longer overlaps titles or the sub-nav.

### Added — observability

- ES `_stats`/`_cat` surfaces and the 101-series Prometheus endpoint
  reflect real load (docs, bytes, search/indexing counters); slow-query
  log; structured logging minors; `_cat/indices` uuid + bytes columns and
  ES-shaped snapshot responses.

### Changed — performance

- **kNN flipped:** HNSW-served top-level kNN — official benchmark cell
  23,325 ms → 1.87 ms at recall@10 1.00 (vs ES 0.80).
- **Date-filtered aggregations:** 41–49× (one-day window 9.9 s → 241 ms)
  via keyword/date columnar range predicates; filtered `extended_stats` /
  `percentiles` / `percentile_ranks` / `median_absolute_deviation` served
  columnar with filter-aware gathers (11–264×).
- **Scored-columnar family at the ES floor:** multi_match, query_string,
  fuzzy, prefix/wildcard, highlight, match_phrase, deep pagination,
  `more_like_this`, `function_score`, composite aggs, `rare_terms` /
  `significant_terms` / percentile families — full-matrix result
  52 WIN / 0 LOSE / 25 TIE against live ES 8.13.4.
- Mixed read-under-write hardening: one memtable walk per query, flush cap,
  merge-publish count seeding, open-loop iso-load writer for honest
  measurement.

### Fixed — autoindex & agent search path

- `xerj autoindex` no longer aborts the whole run on ordinary UTF-8 in the
  SQL-dump sniffer (byte-buffer accumulation; junk files are skipped and
  recorded, never fatal) and no longer mojibakes non-ASCII SQL values.
- `highlight` is applied before `_source` filtering, so fragment-only
  responses work (measured: 3.2× fewer tokens into an agent context at
  equal recall).

### Docs

- Honesty ledger: canonical audited scorecard, ROADMAP claims flipped to
  measured reality, phantom-claim purge across README/site/docs.
- Production recipes: TLS + auth hardening, air-gapped deploy, ES→XERJ
  migration.

## [1.0.0-rc.3] - 2026-07-10

Third release candidate. Headline: XERJ gains a **built-in neural embedder** —
real in-process BERT semantics with no Python and no external service — behind a
single backend-agnostic embedding handle, plus two new end-to-end-validated
retrieval recipes.

### Added

- **Built-in neural BERT embedder — shipped in the binary.** A pure-Rust sentence
  encoder via `candle` (default `all-MiniLM-L6-v2`, 384-dim) that runs in-process
  and **downloads its weights (~90 MB) automatically on first use** (or reads them
  from `embedding.local_model_dir` for air-gapped deployments). It is compiled into
  the default release binary — end users just add `--embed-mode neural` at runtime,
  no special build and no separate binary. A progress bar and one-time-download log
  make the first run legible. The binary is ~36 MB as a result; a
  `--no-default-features` slim build without the neural backend is ~23 MB.
- **Unified three-backend embedding handle (`xerj_ai::Embedder`).** `semantic_text`
  ingest and `semantic`/`hybrid` queries run through one of three interchangeable
  backends — **lexical** (default, zero-dep feature-hash), **neural** (built-in
  BERT), or **proxy** (external OpenAI-compatible `/v1/embeddings`) — selected with
  `embedding.mode`, the `--embed-mode` flag, or `XERJ_EMBED_MODE`. Misconfiguration
  degrades to lexical, never a crash; `auto` preserves the historical behaviour.
- **Recipe — All-you-can-eat search.** One corpus retrieved five ways from a single
  index: full-text (BM25), semantic, vector kNN (more-like-this), hybrid (RRF), and
  semantic-scoped-by-keyword-filter. Guide `docs/recipes/all-way-search.md`,
  runnable `recipes/all_way_search.py`.
- **Recipe — Zero-config folder → neural semantic search.** `xerj autoindex` a
  mixed-format folder against a `--embed-mode neural` server, then search the
  discovered prose by meaning while structured files stay exactly filterable. Guide
  `docs/recipes/autoindex-semantic-search.md`, runnable `recipes/autoindex_semantic.sh`,
  sample corpus `demo/data/support-folder/`.

### Changed

- `--embed-mode {lexical|neural|proxy|auto}` CLI flag and `XERJ_EMBED_MODE` env on
  the server; new `embedding.{mode,neural_model,model_cache_dir,local_model_dir}`
  config keys.
- Documentation updated for honesty consistency (README, AGENTS.md, ROADMAP.md,
  llms.txt, recipe guides): the **default** embedder is lexical; the neural embedder
  is an **opt-in** upgrade — output is only described as neural when that mode runs.

## [1.0.0-rc.1] - 2026-07-06

First public release candidate of XERJ — an Elasticsearch-wire-compatible search,
vector, and log-analytics engine written in Rust and licensed under Apache-2.0. This
is a release candidate: the wire protocol and on-disk format are considered stable
for evaluation, but may still change before the final 1.0.0.

### Added

- **Elasticsearch-compatible REST API.** Drop-in wire compatibility with the ES
  8.x HTTP surface, served from `xerj-api` (`es_compat.rs`) on port `9200`:
  - Document APIs: `PUT`/`GET`/`DELETE /{index}/_doc/{id}` and
    `POST /{index}/_update/{id}`.
  - Search: `POST /{index}/_search` with `query`, `from`, `size`, `sort`, `aggs`,
    `_source`, and `highlight`.
  - Bulk API: `POST /_bulk` with `index`, `create`, `update`, and `delete` actions.
  - Scroll API: `POST /{index}/_search?scroll=1m` and `POST /_search/scroll`.
  - `POST /{index}/_delete_by_query`, index templates (`PUT /_index_template/{name}`),
    and aliases (`POST /_aliases` with `add`/`remove`).
- **Full-text search (`xerj-fts`).** BM25 scoring with an analyzer registry and
  on-disk postings lists. Supported query types include `match_all`, `match_none`,
  `match`, `match_phrase`, `match_phrase_prefix`, `multi_match`, `term`, `terms`,
  `range`, `prefix`, `wildcard`, `exists`, `ids`, `bool`, `fuzzy`, `regexp`,
  `query_string`, `simple_query_string`, `constant_score`, `boosting`, `dis_max`,
  and `geo_distance`.
- **Vector search (`xerj-vector`).** Dense-vector HNSW index for k-NN and semantic
  search, exposed through the `knn`, `semantic`, and `hybrid` query types.
- **Aggregations.** `terms`, `stats`, `avg`, `sum`, `min`, `max`, `value_count`,
  `cardinality`, `range`, `histogram`, `date_histogram`, `percentiles`, `filter`,
  `missing`, and `composite`, with a columnar fast path for `size: 0` aggregations.
- **Sharded ingest and storage (`xerj-storage`).** Write-ahead log with a single
  monotonic sequence-number writer, a 16-shard in-memory memtable
  (`shard = xxh3_64(doc_id) & 15`), flush to immutable segments, and background
  segment merging. WAL replay rebuilds both the storage and FTS memtables on restart.
- **Log analytics (`xerj-logs`).** Columnar log ingestion with retention.
- **AI helpers (`xerj-ai`).** Text chunking, an embedding proxy, and a memory store
  for semantic workflows.
- **Clustering (`xerj-cluster`).** Embedded Raft consensus for cluster metadata with
  no external dependencies.
- **Bundled console (`xerj-console-api`).** Dashboards, auth, preferences, and
  cluster awareness, compiled into the `xerj` binary and mounted under
  `/_xerj-console/api/v1/*`.
- **Transform pipeline (`xerj-wasm`).** Built-in transform plugins with an optional
  WASM backend.
- **Block compression (`xerj-compress`).** LZ4 and Zstd codecs for segment blocks.
- **Single static binary.** `cargo build --release -p xerj-server` produces `xerj`;
  run with `./target/release/xerj --data-dir ./data --insecure`.
- **ES-YAML conformance harness.** A workspace test runner (`es-yaml-runner`) that
  executes the ES 8.13 REST-API-spec YAML suites (search, aggregations, vectors,
  bulk, indices, scroll, cluster) against a live server. XERJ passes 1,326 of 1,329
  cases.
- **Reproducible head-to-head benchmarks.** A 91-cell XERJ-vs-Elasticsearch-8.13
  matrix (ingest, read, vector, and disk dimensions), published and reproducible at
  <https://xerj.org/benchmarks>. The scorecard is honest about both wins and losses.

### Changed

- `_forcemerge` is now synchronous and quiescent, matching Elasticsearch semantics,
  and merge status is exposed through `_stats`.
- Search hit materialization for `size > 0` is bounded to the top `from + size`
  candidates, reducing per-query cost from O(N) toward O(from + size).
- Bulk ingest avoids redundant JSON round-trips and batches schema evolution to
  raise throughput under concurrent load.

### Fixed

- Consecutive `_bulk` `delete` actions that were previously dropped are now applied
  correctly.
- `hits.total` for `size > 0` searches is delete-aware, resolving a conformance
  regression.
- Corrected top-N sort behavior and delete-awareness across the memtable/segment
  merge path.

### Known limitations

- 3 of 1,329 ES-YAML conformance cases do not yet pass.
- This is a release candidate; some Elasticsearch APIs and query/aggregation options
  outside the list above are not yet implemented. See
  [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) and `engine/CLAUDE.md` for the
  current supported surface.

[Unreleased]: https://github.com/xerj-org/xerj/compare/v1.0.0-rc.5...HEAD
[1.0.0-rc.5]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.5
[1.0.0-rc.4]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.4
[1.0.0-rc.3]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.3
[1.0.0-rc.2]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.2
[1.0.0-rc.1]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.1
