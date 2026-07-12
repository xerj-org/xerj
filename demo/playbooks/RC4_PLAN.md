# RC4 Release Plan — XERJ v1.0.0-rc.4

_Date: 2026-07-12. Owner: RC4 release owner. Baseline: v1.0.0-rc.3 (main @ 4c69c05+)._

Synthesized from 9 structured production-readiness reviews (docs-roadmap, engine-search,
engine-aggs, api-escompat, storage-durability, fts-query-vector-ai, server-ops-security,
observability-limits, usecases-e2e), each backed by file:line evidence and/or live probes
of rc.3 on :9200 vs ES 8.13.4 on :9201.

**Bar (strict ordering): correctness > durability > stability/resource-limits > security > observability > features/docs.**

**Wave 1 = 17 blockers.** Waves are sized for the parallel-worktree workflow used in RC3
(see memory: RC3 progress; reusable script + integration recipe). Build discipline:
`cargo build --release -j 32 -p <touched-crate-chain>` only — never workspace-wide, never clean.

---

## Flagged product decision (not a task)

- **[DECISION] Mixed read-under-write visibility mode** — product call pending (memory:
  mixed-p99 root cause = live memtable under writer lock; see
  `demo/playbooks/MIXED_READ_UNDER_WRITE_FINDING_2026-07-08.md`). Gates: how the 4 honest
  mixed-RUW LOSE rows are annotated in the scorecard (Wave 4 item 8) and the scope of the
  "Any LOSE fails CI" sentence in SCORECARD.md. Decide before Wave 4 doc propagation;
  no engine work in rc4 hangs on it.

---

## Wave 1 — Blockers only (17)

Grouped into 6 parallel worktree streams. Every item would embarrass or endanger a
production deployment.

### Stream A — remote crashes (correctness/stability)
1. **[BLOCKER][M]** Highlight `number_of_fragments:0` SIGABRTs the whole process on text whose lowercasing changes byte length (İ → 2 bytes); non-crashing cases return corrupted `<em>` offsets — fix matching to be offset-correct against original text, not just crash-safe. Evidence: `engine/crates/xerj-engine/src/index.rs:14727` slicing original text with `text_lower` offsets (built at :14577); live exit 134, reproduced twice.
2. **[BLOCKER][M]** Painless parser/evaluator recursion has no depth guard — a ~3KB nested-parens script in `script_score` stack-overflows and aborts the server (unauthenticated remote crash); add depth cap (~100) + max source length across parser, evaluator, and `aggs.rs parse_script`. Evidence: `engine/crates/xerj-engine/src/painless.rs:463-660` (parse), `:728-950` (eval); live exit 134 twice.

### Stream B — silent wrong data (correctness)
3. **[BLOCKER][M]** Bulk `index` action never validates doc-body JSON: malformed NDJSON is stored as an empty `{}` doc with status 201, `errors:false` (dominant ingest path, silent data loss) — validate on the turbo-raw path or at drain-for-flush with per-item 400. Evidence: `engine/crates/xerj-engine/src/bulk.rs:540` (parse skipped for `index`; `create`/`update` parse at :546); live probe.
4. **[BLOCKER][M]** `match`/BM25 over `semantic_text` fields silently returns 0 hits once docs flush to segments (FTS sidecars exist but the segment query path skips them) — breaks the flagship all-way-search lexical/hybrid legs and memory-API lexical recall; add flush-then-assert regression test. Evidence: live repro (1 hit pre-flush → 0 post-flush; `semantic` still works); `docs/examples/all-way-search/all_way_search.py:138,175`.
5. **[BLOCKER][M]** Snapshot restore ignores the `indices` filter and rewrites EVERY index in the cluster with snapshot-time state (clobbers post-snapshot writes) — honor the filter or 400 on filtered bodies; return ES-shaped restore info. Evidence: live probe restored all 30 index dirs (mtime 14:08:22) when asked for `products` only.
6. **[BLOCKER][S]** `_reindex` silently ignores `source.remote` and reindexes the same-named LOCAL index, reporting success — the standard ES→XERJ migration path returns wrong data; minimum fix: deny-unknown-fields / explicit 400 "reindex from remote is not supported". Evidence: `engine/crates/xerj-api/src/es_compat.rs:13297`; live probe never contacted :9201.
7. **[BLOCKER][S→M]** Top-level kNN family: `knn.filter` silently dropped (returns docs the filter must exclude — canonical ES vector-search shape); `knn.similarity` threshold ignored (returns hits ES excludes); `knn:[...]` array form silently returns 0 hits (defaults field to "embedding") — thread filter+boost through `knn_body_to_query_node`, apply the similarity cutoff, accept the array form (or 400). Evidence: `es_compat.rs:21516` (`filter: None` hardcoded), `:21482-21519` (similarity never read), `:4831` (array treated as object); all three live-diverged vs ES.

### Stream C — durability (acked-write loss)
8. **[BLOCKER][M]** Acked-write LOSS on process crash (live-proven: 50/50 acked docs gone after kill -9 racing `_flush`): WAL maintenance checkpoints with the global seq counter + full file offset, then `prune()` deletes generations still holding acked-but-unflushed entries — bound prune by per-generation max seq ≤ checkpoint max_seq, checkpoint drain-time per-shard offsets, and flush must checkpoint `snapshot.max_seq_no`, never `current_seq_no()-1`; add bulk-during-flush + kill -9 regression. Evidence: `engine/crates/xerj-engine/src/index.rs:8847-8850`, `xerj-storage/src/wal.rs:551-555`, `:610-621`, `index_store.rs:1497-1525`.
9. **[BLOCKER][S]** `wal_sync = "sync"` is silently ignored on all bulk paths (mode forcibly overridden to Batched; fsync only via env `XERJ_STRICT_SYNC`) and `wal_batch_ms` is documented but entirely unimplemented — honor the operator's explicit durability opt-in; implement the batched-fsync loop or delete the knob; warn loudly on `XERJ_SKIP_WAL`. Evidence: `index_store.rs:2317-2329`, `:2478-2514`; `xerj-common/src/config.rs:328-361`; `xerj.default.toml:85-87`.
10. **[BLOCKER][M]** Segment publish chain is not power-loss ordered: `snapshot.json`/`.ids`/`.dv`/FTS sidecars written without fsync (no dir fsync after `.seg` rename) while the WAL is pruned ~1s later — power loss can GC flushed, WAL-pruned segments as orphans (acked-data-loss class). Route all sidecar writes through the existing `write_file_atomic` pattern (`index.rs:15035-15049`) and make prune conditional on the fsync barrier. Evidence: `index_store.rs:1869-1876`, `:763-768`; `index.rs:9407-9415`; `xerj-fts/src/index.rs:771,815,855-863`; `segment.rs:387-396`.

### Stream D — stability / resource limits
11. **[BLOCKER][M]** Scroll and async-search contexts are never TTL-swept and have no open-context cap; each scroll pins a fully-hydrated `Vec<Hit>` forever — unauthenticated (default `--insecure`) unbounded-memory DoS from normal client behavior; mirror the existing PIT sweeper, enforce keep-alive on continuation, add a max-open cap (429). Plausible RSS-runaway contributor. Evidence: `engine.rs:97-103` (created never read), `:366` (PIT-only sweeper); `es_compat.rs:13212`, `:22483` (expiry stored, never enforced).
12. **[BLOCKER][L]** Search `timeout` can never fire on the production runtime — `block_in_place(block_on(...))` completes the whole search on first poll, so runaway queries are uncancellable (live: `timeout:1ms` regexp ran 7.7s, `timed_out:false`); implement cooperative deadline checks every N docs in scan/agg loops, return partials with `timed_out:true`. Evidence: `index.rs:5286-5296`; dead handling at `:5371-5381`.
13. **[BLOCKER][S]** No data-dir exclusivity lock — a second xerj process opens a live data dir, replays WAL, and flushes segments into it (week-one corruption via systemd double-start); take exclusive flock on `<data-dir>/node.lock` before any replay/write, fail fast with pid. Evidence: live boot log (bind failed EADDRINUSE yet wrote 4 segments into the served dir); no flock anywhere in engine crates.
14. **[BLOCKER][S]** Shipped `engine/xerj.default.toml` fails to parse with the rc.3 binary (`unknown field hnsw_offload_threshold`, line 223) — the documented `--config` invocation is a dead boot; fix key or deserializer + CI smoke test booting the binary with the shipped config. Evidence: live boot failure; independently found by 2 reviews.

### Stream E — security
15. **[BLOCKER][M]** gRPC listener (:8081) is plaintext h2c AND fully unauthenticated even with `auth.enabled=true`, bound 0.0.0.0 by default — full unauthenticated read/write/delete; add a tonic auth interceptor matching `auth_middleware`, or refuse non-loopback bind in secure mode. Evidence: `engine/crates/xerj-server/src/grpc.rs:46-261` (no auth in any handler); live connect on auth-enabled instance.
16. **[BLOCKER][S]** `/health/live` + `/health/ready` return 401 when auth is enabled — the documented Docker HEALTHCHECK and Helm probes crashloop the pod exactly when hardening is applied; exempt probe paths from the auth layer. Evidence: live 401s; `router.rs:96-99,191-192`; `Dockerfile:22`; `deploy/helm/xerj/values.yaml`.

### Stream F — docs truth (AI-facing contract)
17. **[BLOCKER][M]** kNN claims are factually false in both directions on every AI-reader surface: "NO approximate HNSW graph traversal" / "recall 1.00 by construction" vs the engine serving kNN through a persisted HNSW graph since c6cbe9f — rewrite the caveat blocks (llms.txt, llms-full.txt, README ×3, ROADMAP) to: HNSW-served approximate kNN, measured recall@10 1.00 on the bench corpus, brute path retained. Evidence: `landing/llms.txt:27`, `llms-full.txt:76-78,205`, `README.md:48,225,255` vs `index.rs:3791` (`run_knn_hnsw`), `:5523`, `:1015`.

---

## Wave 2 — Correctness majors + durability hardening

### Doc CRUD / ES write semantics
1. **[MAJOR][S]** GET `/_doc/{id}` hardcodes `_version:1`/`_seq_no:1`, permanently breaking read-then-CAS — pass real values (the source `_mget` already uses). Evidence: `es_compat.rs:1840`; live (real seq_no 6, GET says 1).
2. **[MAJOR][M]** Repeated PUT to same `_id` always returns `result:"created"` and `_version` jumps by 2 — return `updated`/200 for existing ids and monotonic per-doc versions. Evidence: live versions 2,4,6 all "created" vs ES 1,2,3.
3. **[MAJOR][M]** Conditional DELETE (`if_seq_no`/`if_primary_term`) silently ignored, and DELETE of a missing doc returns 200/`deleted` instead of 404/`not_found`. Evidence: `es_compat.rs:1912` (params dropped), `:1928` (always deleted); live 200 vs ES 409/404.
4. **[MAJOR][S]** POST `/{index}/_doc/{id}` returns 405 (and no auto-create) — add the POST route to the PUT handler. Evidence: live 405; `router.rs:259` has no `.post`.

### Aggregations / scripting
5. **[MAJOR][M]** `terms` + `multi_terms` always emit `sum_other_doc_count: 0` even when `size` truncates (fast AND brute paths hardcoded) — the flagship agg is silently wrong in the common case. Evidence: `aggs.rs:2830`, `fast_aggs.rs:2681`, `aggs.rs:8790`; live 0 vs ES 901269.
6. **[MAJOR][S]** Painless string comparisons coerce both operands to 0.0 — every string compares equal (`doc['color'].value == 'red'` matched all docs live); compare Strings as strings. Evidence: `painless.rs:799-824`.
7. **[MAJOR][M]** Composite agg key typing wrong: boolean source → `"true"` string (known ticket) AND keyword `"007"` → number 7 (data corruption) — type keys from the source field mapping, never string-parse heuristics. Evidence: `aggs.rs` run_composite ~6393; live diverged vs ES.
8. **[MINOR][S]** `multi_terms` silently drops buckets past the 65536 cap with no error — raise `too_many_buckets` like `date_histogram`/`histogram` already do. Evidence: `aggs.rs:8745` vs `:4487/5545`.

### Query/date correctness debt (known-ticket bundle)
9. **[MAJOR][M]** Date-parse debt family (known tickets, absorbed here): gt/lte unit rounding, range `format` param ignored + bogus format → 200, date-math `||+1M/d`, partial `"2026-02"` bounds — one worktree, ES-diff regression tests per case.
10. **[MAJOR][S]** PIT search response omits `pit_id`, breaking the canonical PIT+search_after loop — echo the (refreshed) id. Evidence: live probe; snapshot isolation itself verified OK.
11. **[MINOR][S]** Pinned/beyond-end population `max_score` on empty pages (known ticket) — small formatting-correctness fix alongside 10.
12. **[MAJOR][S]** `/_memory/{ns}/_recall` fail-silent body: unknown keys ignored → degrades to match-all at score 1.0 (absorbs the known query-vs-text field-mismatch ticket; recency blending is now real — `memory_api.rs:389,412` — close that half) — deny unknown fields, require exactly one of `vector|query`. Evidence: live `{"zzz":...}` returned 5 memories @1.0.

### Storage/durability hardening
13. **[MAJOR][M]** Disk-full/transient write error poisons the WAL generation: replay silently drops every acked entry after the first torn frame — truncate/rotate on append error; hard-error (or boundary-resync) on mid-file CRC mismatch followed by parseable frames; ENOSPC injection test. Evidence: `wal.rs:416-429`, `:948-954`, `:846-850`.
14. **[MAJOR][M→L]** Delete-heavy WAL retention pinning (known ticket, live-confirmed: one plain delete pins its shard's prune forever) — interim: unpin once the delete's tombstone is segment-resident; full Option-A segment tombstones + seq-aware reopen apply as stretch. Evidence: `index_store.rs:369-380`, `:1595-1625`, write-only Tombstones section `:1293-1304`.
15. **[MAJOR][S]** Transient GET 404 under merge (known ticket, now root-caused): version_map repointed to the merged segment before snapshot publish — fall back to `open_segment_arc(seg_id)` when the id is absent from the snapshot; also remove the exists()-then-open TOCTOU (`store_exception` file-race flag). Evidence: `index.rs:3126-3136` vs `:3255`, `:4914-4945`; `index_store.rs:1790-1795`.

### Vector/AI correctness & resource hygiene
16. **[MAJOR][S]** HNSW graph auto-built + persisted for ANY unmapped numeric array (`ports:[80,443]` built a 585KB graph, full f32 slab in RAM forever; never serves queries) — delete `choose_hnsw_field` heuristic 3, require a dense_vector mapping. Feeds the RSS-runaway ticket. Evidence: `index.rs:3677-3684`; live probe.
17. **[MAJOR][M]** Unclean shutdown permanently disables ANN: `stale=true` is never cleared (no rebuild path), while ingest keeps paying full graph cost — rebuild on open/background from WAL-tail vectors, surface staleness in stats. Evidence: `index.rs:1021-1035,1079,4777-4778`; live probe.
18. **[MAJOR][S]** Semantic chunker silently omits up to 64 chars between chunks (gapped text missing from pooled + passage vectors) — advance from the actual break; add a real coverage assertion. Evidence: `xerj-ai/src/chunker.rs:125-134`; simulated 16-char gap.
19. **[MINOR][S]** `HnswIndex::save_to` renames without fsync — power loss drops the snapshot into the permanent-stale/brute path; use the write_atomic pattern. Evidence: `hnsw.rs:785-786`.

### Server correctness quick fixes
20. **[MAJOR][S]** Periodic background flusher aborted immediately after spawn (`flusher.abort()` before `join!`) — `flush_interval_secs` is inert; idle memtables/WAL grow until shutdown (compounds WAL retention). Evidence: `main.rs:1133` vs `:1168`.
21. **[MAJOR][S]** `admin.key` and TLS private key written 0664 (world/group-readable) — chmod 0600 after write, same as the console master key (`bootstrap.rs:127-130`). Evidence: live stat 664; `main.rs:296,336`.

---

## Wave 3 — Resource governance, security posture, config honesty, validation

1. **[MAJOR][L]** No global memory budget / parent circuit breaker: memtable backpressure is per-index only (N indices × 1.5GB, no ceiling), `max_query_memory_mb` has ZERO enforcement sites — add a process-wide memtable byte budget + RSS-threshold admission check (429 `circuit_breaking_exception`), wire `max_query_memory_mb` into agg/hydration accounting or fail loud on the inert knob. The structural fix for the 112GB OOM class. Evidence: `index.rs:1374-1490,1949-1980`; `config.rs:697` (no readers); `error.rs:200`.
2. **[MINOR][S]** `max_concurrent_searches` config is dead — semaphore hardcoded `Semaphore::new(64)` per index, no global cap; wire the knob + a global pool. Evidence: `config.rs:699` vs `index.rs:845,1057`.
3. **[MAJOR][M]** No disk watermarks — engine writes until ENOSPC (which Wave 2 #13 shows poisons the WAL); background statvfs check + auto write-block at flood stage, mirroring ES thresholds. Evidence: statvfs exists only for `_cat/allocation` (`es_compat.rs:17649`); `index.rs:1179` write-block machinery exists.
4. **[MAJOR][M]** Request-validation bundle (silent-wrong-query class): `deny_unknown_fields` on `EsSearchBody` (bogus keys → 200 today); search_after arity/sort validation; rescore+sort reject; `size=abc` → ES-shaped JSON error, not text/plain axum rejection; undecodable scroll_id → 400 (not 404 with the sentence stuffed in `root_cause[].index`); `index_not_found` resource.type/id fields. Evidence: `es_compat.rs:1943`; live probes vs ES on all shapes.
5. **[MAJOR][S]** CORS hardcoded allow-any origin/method/header on both routers with no config knob — make origins configurable, default restrictive. Evidence: `router.rs:707-712` (2 reviews).
6. **[MAJOR][M]** API-key model: every key is superuser (role_descriptors accepted, never enforced) and keys are in-memory only (all minted keys die on restart) — persist keys; gate `/_security/role*` behind 501 or document loudly; full RBAC enforcement deferred (see below). Evidence: `auth.rs:65-94`; `engine.rs:209`; `rbac.rs:1-15`.
7. **[MINOR][S]** Admin key compared with early-exit `==` while created keys use constant_time_eq — use the same helper. Evidence: `auth.rs:52` vs `:93`.
8. **[MAJOR][M]** ANN all-or-nothing coverage gate: one doc lacking the pinned vector field keeps the whole index on O(N) brute forever, silently — gate on `vector_doc_count` instead of total doc_count; expose coverage in stats. Evidence: `index.rs:3824,3567-3572`.
9. **[MINOR][S]** Embedding-proxy outage surfaces as 400 `invalid_query` and retries non-transient 4xx (up to ~2min/doc stall) — classify errors, fail fast on 4xx, map to 5xx class. Evidence: `index.rs:4338-4342`; `embed.rs:44-52,157-171`.
10. **[MINOR][S]** No data-dir format version marker; unparseable `snapshot.json` silently treated as empty (all segments become orphans) — write a version/meta file, `#[serde(default)]` hygiene on SegmentMeta/IndexSnapshot, rc3-fixture cross-version open test in CI (matters for the rc3→rc4 upgrade itself). Evidence: `segment.rs:238-251`; `index_store.rs:419`.
11. **[MINOR][S]** Storage-crate `MergeExecutor` produces Stored-only segments and repoints in the wrong order (public API footgun, unused by the server) — delete or `#[doc(hidden)]`. Evidence: `merge.rs:316-340`; `lib.rs:43`.
12. **[MINOR][S]** Memory API `list()` silently truncates at 100 with no cursor; no per-namespace authorization — add after/from cursor; document the auth model. Evidence: `memory_api.rs:51`; `router.rs:660`.
13. **[MINOR][S]** API-fidelity minors bundle: `_cat/indices` mints a random uuid per request (emit the real settings uuid) + human byte sizes; add `/_cat/indices/{pattern}` route (404 today, workaround shipped inside our own example); snapshot response ES shape (drop 170KB `index_files`, real duration, exclude `.xerj_*` system indices by default). Evidence: live probes; `migrate_demo.sh:116-123`.

---

## Wave 4 — Observability + truthful docs/claims

### Observability (top of wave — silent-fake class)
1. **[MAJOR][M]** ES-compat monitoring stats are silent-fake zeros: `_nodes/stats` indexing/search totals, thread_pool, GC hardcoded; per-index `_stats.search.query_total: 0` (live-verified after a search) — plumb the already-existing per-index counters (`index.rs:501-503`, exposed at `:8957-8958`) + add ingest counters; report real thread counts; omit what's not real per the honesty policy. Every ES dashboard flatlines today. Evidence: `es_compat.rs:15005-15006,14990-15057,12485`. _(Reviewer classified BLOCKER per the silent-fake policy; release-owner call: MAJOR — misleading, not data-endangering — but it ships in rc4.)_
2. **[MAJOR][M]** 12 of 17 declared Prometheus metrics are dead (never recorded), including exactly the gauges needed to see the open RSS-runaway and WAL-growth tickets — wire flush/merge/WAL-latency observers at their call sites; 10s background task sets doc_count/segment_count/wal_size_bytes/memory_usage (read_rss_bytes exists); use `record_query`/`active_search_guard` (currently test-only). Evidence: live scrape all zeros at 2.37M docs; `metrics.rs:314,331`.
3. **[MINOR][S]** Slow-query log: native search path never feeds it; entries omit the query body; tracing thresholds independent of the runtime-settable one — unify. Evidence: `es_compat.rs:6834` only; `slow_query.rs:37-50`; `index.rs:5305-5320`.
4. **[MINOR][S]** Mount `/v1/metrics` on the ES-compat router (:9200-only deployments can't scrape today); optional read-only metrics token; add query-cache hit/miss counters. Evidence: live :9200 404; `router.rs:101`.
5. **[MINOR][S]** Per-index metric labels unbounded (nonexistent-index queries mint series forever) — record after index resolution; prune labels on delete. Evidence: live `no_such_index_xyz` series.
6. **[MINOR][S]** JSON structured-log option + INFO-level access log via TraceLayer config. Evidence: `main.rs:385-390`; no `[logging]` config section.
7. **[MINOR][S]** Expose HNSW stale flag, tombstone_count, vector coverage in index stats/_cat so ANN-off states stop being silent. Evidence: `hnsw.rs:493-495` (available, unexposed).

### Docs / claims (honesty ledger)
8. **[MAJOR][M]** Commit the in-flight SCORECARD.md/BENCHMARK_VS_ES.md and propagate ONE canonical scorecard (55W/4L/26T incl. the 4 honest mixed-RUW losses — pending the [DECISION] annotation) to README, llms.txt, llms-full.txt, landing — replacing stale 42W/28L/12T everywhere. Evidence: README.md:108,237; llms.txt:65; landing/index.html:328.
9. **[MAJOR][M]** Full ROADMAP.md re-review against rc.4 HEAD: flip the five now-shipped "Partial" entries (percolate, scripted_metric, significant_terms, has_child fail-loud, memory recency), 1,326→1,360 conformance, 23MB→36MB, add Landed-since-rc-3 section, restamp. Evidence: ROADMAP.md:5,11,19,36,73-87.
10. **[MAJOR][M]** Remove/replace unsubstantiated landing perf claims (21×/300×/56×/13MB/50ms) on public-sector, solutions, resources, brandbook pages — mandated by ROADMAP.md:100 and the bench-battle honesty ledger; use the reproducible closed-loop numbers.
11. **[MINOR][S]** STUB_AUDIT.md: add ✅ FIXED markers (TLS in-process, percolate, has_child, scalar8 quantization) + re-count headline; ES_COMPATIBILITY.md: short §16 rc.3/rc.4 addendum (TLS, percolate, script_fields, HNSW kNN, 1360/1363).
12. **[MINOR][S]** Quick doc-fix bundle: README badge rc.1→rc.4; llms.txt 23MB→~36MB; SCORECARD stale percolate note + scope the "Any LOSE fails CI" sentence; llms.txt ingest-caveat wording refreshed (keep the caveat — RSS runaway confirmed OPEN — drop stale 42GB/4.5M figures); ROADMAP.md:36 drop the now-false memory-recency limitation; aggs.rs:11-14 docstring after Wave 2 #5 lands.
13. **[MAJOR][M]** Ops/user docs that unblock production adopters: `docs/recipes/production-deployment.md` (TLS/auth quickstart — every recipe currently boots `--insecure`); air-gapped neural-model pre-seed doc; interim ES→XERJ migration recipe (scroll+bulk, since remote reindex now fails loud); single-node production posture next to `cluster.enabled` (unauth plaintext Raft transport = experimental).
14. **[MINOR][S]** Document the durability posture explicitly: batched WAL = process-crash durable, NOT power-loss durable (below ES request-fsync default); `wal_sync="sync"` (now honored, Wave 1 #9) is the power-loss opt-in.

---

## Defer / wontfix for RC4 (with reasons)

- **BM25 cross-segment IDF divergence** (per-segment N/df on unmerged/multi-shard small indices; self-heals after merge) — XL; defer the dfs-style stats aggregation, DOCUMENT in Wave 4 that ES-identical relevance holds post-merge/single-segment. Evidence: `xerj-fts/src/bm25.rs:76-79`; live 0.28768 vs ES 0.18232.
- **Full RBAC enforcement** — XL; startup banner already honestly admits "no RBAC"; Wave 3 #6 stops the endpoints implying otherwise. Post-rc4.
- **HNSW tombstone compaction** (update-heavy graph RAM growth) — L; Wave 4 #7 exposes tombstone_count; compaction post-rc4. Linked to RSS-runaway investigation.
- **HNSW build off the ingest hot path** (33× ingest slowdown at 64 dims) — L; results are correct, cost documented in Wave 4; queue/batch-build post-rc4.
- **Semantic query path via HNSW + skip-unchanged graph saves** — M but perf-only (brute is correct); post-rc4.
- **Streaming segment writer / bounded merge materialization** — L; OOM-history contributor but capped today; post-rc4 with the global budget (Wave 3 #1) as the rc4-era backstop.
- **Columnar fast paths for the ~15 brute-only agg families** (string_stats 18s, multi_terms 21s @2.3M) — L; correct-but-slow; document scale limits; consider an agg work budget post-rc4.
- **Remote reindex implementation** — L; Wave 1 #6 makes it fail loud, Wave 4 #13 ships the working interim recipe.
- **Raft/cluster transport auth+TLS** — cluster is not in the supported single-node production posture; documented in Wave 4 #13.
- **Deep `from=` full hydration** — bounded by max_result_window=10k (worst case ~14ms live); known ticket stays open, not rc4.
- **Response-formatting cosmetics** (known tickets: `_source` not byte-verbatim, float exponent `e+38` case, `_explain` brute explanations) — MINOR, no client breakage evidenced; post-rc4. (The composite boolean/keyword key half of the family IS fixed in Wave 2 #7.)
- **RSS-runaway ticket (historic 112GB OOM)** — NOT closed, repro still unclear; rc4 removes identified mechanisms (Wave 1 #11 scroll leak, Wave 2 #16/#17 HNSW waste, Wave 3 #1 global budget) and makes it observable (Wave 4 #2). Investigation stays open.

---

## Known-ticket disposition

| Ticket | Disposition |
|---|---|
| RSS runaway (112GB OOM) | OPEN — mechanisms addressed W1#11/W2#16-17/W3#1; observable via W4#2 |
| Transient GET 404 under merge / store_exception race | W2 #15 (root-caused, S) |
| Date-parse debt family | W2 #9 (bundled worktree) |
| Response-formatting family | Composite key → W2 #7; rest deferred |
| pinned/beyond-end max_score empty pages | W2 #11 |
| _explain brute explanations | Deferred |
| Deep-from full-hydrate | Deferred (bounded) |
| WAL retention on delete-heavy indices | W2 #14 (interim unpin; Option-A stretch) |
| Batched WAL not power-loss durable | By design — W1 #9 makes the opt-in real; W4 #14 documents |
| Memory-API text-recall mismatch + recency | Recency RESOLVED in code; field/fail-silent → W2 #12; ticket close in W4 #12 |
| Mixed read-under-write visibility mode | [DECISION] — flagged above, gates W4 #8 annotation only |

---

## Execution notes

- Parallel worktrees per stream, RC3-style (memory: rc3-progress — reusable script + integration recipe). Wave 1 streams A-F are file-disjoint enough to run 6-wide.
- Scoped builds only: `cargo build --release -j 32 -p <touched-crate-chain>`.
- Every Wave 1/2 correctness item lands with an ES-diff regression test (live :9201 reference) and, where a crash/durability repro exists, the exact live repro from its review as the test.
- Gate for cutting rc4: Wave 1 complete + 1360/1363 YAML suite green + kill -9-during-flush durability test green + shipped-default-config boot smoke green.
