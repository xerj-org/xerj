# Changelog

All notable changes to XERJ are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed — fail-closed autoindex restart semantics

- **`autoindex --fresh` no longer discards a durable resume plan.** A durable
  plan contains the alias, path, graph, and stale-record knowledge needed for
  safe reconciliation. `--fresh` is accepted only when the selected state
  directory has no durable plan and never cleans the destination. An
  independent rebuild must use a new state directory, new prefix, and new brain
  namespace when graph detection is enabled (or disable graph writes), be
  validated, and only then replace the old reader target. The shared catalog
  and old target require explicit, validated cleanup. Generated journals with
  `--no-graph` reconcile additions, changes, deletions, renames, and no-op
  reruns. Legacy journals and graph-enabled generations refuse membership
  changes before remote mutation.

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
