# Changelog

All notable changes to XERJ are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/xerj-org/xerj/compare/v1.0.0-rc.1...HEAD
[1.0.0-rc.1]: https://github.com/xerj-org/xerj/releases/tag/v1.0.0-rc.1
