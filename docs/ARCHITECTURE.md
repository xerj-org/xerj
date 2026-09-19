# XERJ Architecture

This document orients contributors to how XERJ is put together: the crate layout,
the path a search request takes, and the path an indexed document takes. It is a
map, not a specification — the source is authoritative, and
[`AGENTS.md`](../AGENTS.md) is the canonical quick reference for
build/run/test commands and the supported ES surface.

XERJ is an AI-native search, vector, and log-analytics engine written from
scratch in Rust and published under Apache-2.0 — designed for AI-agent workloads
(zero-config `autoindex` onboarding, agent data map, `/_memory`), with an
independent implementation that shares no code and no architecture with
Elasticsearch or Lucene. It additionally speaks the
Elasticsearch 8.x HTTP protocol as a zero-migration adoption bridge, so existing
ES clients, dashboards, and ingest tooling talk to it unchanged (see
[WHY_XERJ.md](./WHY_XERJ.md) for the design rationale). For a six-axis,
source-linked comparison with Lucene, see [XERJ vs Lucene](./XERJ_VS_LUCENE.md).

## Bird's-eye view

```
            ┌───────────────────────────────────────────────┐
  ES client │  HTTP :9200  (ES-compatible + native REST)     │
  ────────► │  xerj-server → xerj-api (Axum)                 │
            └───────────────────────────────────────────────┘
                                  │
            ┌─────────────────────┼─────────────────────┐
            ▼                     ▼                     ▼
     xerj-query            xerj-engine             xerj-console-api
   (parse ES JSON →   (Engine / Index: ties       (bundled dashboards
    QueryNode tree)    storage + fts + vector)      under /_xerj-console)
                                  │
         ┌────────────┬──────────┼───────────┬────────────┐
         ▼            ▼          ▼           ▼            ▼
    xerj-storage  xerj-fts   xerj-vector  xerj-logs   xerj-compress
     (WAL,        (BM25,      (HNSW        (columnar   (LZ4 / Zstd
     memtable,    analyzers,   k-NN)       logs +       block codecs)
     segments)    postings)                retention)
```

Supporting crates cut across the stack: `xerj-common` (shared `Config`, `Schema`,
`FieldType`, `XerjError`), `xerj-ai` (chunking, embedding proxy, memory store),
`xerj-cluster` (embedded Raft for cluster metadata), and `xerj-wasm` (transform
pipeline plugins).

## Crate responsibilities

| Crate | Responsibility |
|---|---|
| `xerj-server` | Binary entry point: CLI parsing, config loading, starts the API. |
| `xerj-api` | Axum HTTP layer — ES-compatible handlers (`es_compat.rs`) and the native API (`native.rs`). |
| `xerj-engine` | Integration crate: the `Engine` and `Index` structs that tie storage, FTS, vector, and aggregations together. |
| `xerj-query` | Query DSL: AST (`ast.rs`), ES JSON parser (`parser.rs`), planner, rewriter, executor. |
| `xerj-storage` | WAL, sharded memtable, segments, version map, index store. |
| `xerj-fts` | Full-text search: BM25 scoring, analyzer registry, postings lists. |
| `xerj-vector` | Dense-vector k-NN / semantic search: a persisted HNSW graph serves unfiltered kNN with exact rescoring; filtered shapes and every fallback use the exact scan. |
| `xerj-logs` | Columnar log ingestion and retention. |
| `xerj-ai` | Text chunking, embedding proxy, memory store. |
| `xerj-compress` | Block compression codecs (LZ4, Zstd). |
| `xerj-common` | Shared types: `Config`, `Schema`, `FieldType`, `XerjError`. |
| `xerj-cluster` | Embedded Raft consensus for cluster metadata (no external dependencies). |
| `xerj-console-api` | Bundled console backend (dashboards, auth, prefs) mounted at `/_xerj-console/api/v1/*`. |
| `xerj-rerank` | Optional second-stage reranking for `_search`: calls an external relevance judge after hits are rendered. The only search-time component that makes an outbound call with document text (proxy embeddings and the WAL tap also send text off the node when configured; RERANK.md lists every outbound connection); never linked by `xerj-engine`. See [RERANK.md](./RERANK.md). |
| `xerj-wasm` | Pluggable transform pipeline with an optional WASM backend. |
| `tests/es-compat-yaml` | `es-yaml-runner`: executes ES REST-spec YAML suites against a live server. |

## The search path

A search request flows front-to-back through four crates:

```
HTTP POST /{index}/_search
    → xerj-api      Axum handler in es_compat.rs
    → xerj-query    parse_request(): raw ES JSON → SearchRequest (QueryNode tree)
    → xerj-engine   Engine::get_index() looks up the named index
    → xerj-engine   Index::search()
         ├─ memtable scan        in-memory BM25 via ShardedFtsMemtable
         ├─ segment scan         on-disk FTS via FtsIndexReader + BM25
         ├─ doc_matches_query()  term-level / geo predicate evaluation
         ├─ run_aggs()           aggregation pipeline (columnar fast path for size:0)
         └─ apply_source_filter(), apply_highlight()
    → SearchResult → JSON response
```

A query is matched against **both** the in-memory memtable and the on-disk segments,
and the results are merged so that freshly written documents are immediately
searchable. For `size > 0` requests, hit materialization is bounded to the top
`from + size` candidates. k-NN and hybrid queries evaluate the vector portion in
`xerj-vector` (unfiltered top-level kNN via the persisted HNSW graph with exact
rescoring; other shapes via the exact scan) and combine scores in the same executor.

## The ingest path

Writes go through a write-ahead log into a sharded memtable, then flush to immutable
segments that are later merged:

```
PUT /{index}/_doc/{id}   or   POST /_bulk
    → xerj-api → xerj-engine → xerj-storage (IndexStore)
         ├─ WAL append          sharded Mutex<WalWriter>s; global AtomicU64 seq_no
         ├─ storage memtable    rounded power-of-two hash partitions
         ├─ FTS memtable        global engine.ingest_shards hash partitions
         ├─ flush               Index::flush() fans out per-shard do_flush_shard tasks;
         │                      each drains its shard and writes an immutable segment
         │                      with FTS/doc-values sidecars (LZ4/Zstd blocks)
         └─ merge               background segment merge compacts small segments;
                                _forcemerge is synchronous + quiescent (ES-like)
```

The global `engine.ingest_shards` value is validated as a non-zero power of two
no greater than 256 ([global validation](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-common/src/config.rs#L1512-L1570)).
`IndexStore` keeps its WAL writers and storage memtable in separate shard
domains; the storage memtable rounds its configured count up to the next power
of two for hash-mask routing ([construction](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-storage/src/index_store.rs#L660-L720),
[routing](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-storage/src/index_store.rs#L1607-L1643)). The WAL-writer
and rounded storage-memtable counts are distinct domains, not a promised
one-to-one pairing. The engine-side FTS memtable is separate again: create and
reopen pass the global `engine.ingest_shards` into
[`ShardedFtsMemtable`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/memtable.rs#L628-L709)
([create](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L6022-L6026),
[reopen](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L6220-L6224)).
Query paths iterate its shards, and production flush drains each FTS shard
through [`Index::flush`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L18012-L18147)/[`do_flush_shard`](https://github.com/xerj-org/xerj/blob/24711999dd866ceec2a6e7c91d934c2c27d7066c/engine/crates/xerj-engine/src/index.rs#L24433-L24920).

### Recovery

On restart the engine rebuilds state from disk with no external coordinator:

```
Engine::new() scans data_dir/
    → for each index dir: Index::open()
         ├─ IndexStore::open()    replays the WAL into the storage memtable
         ├─ WalReader::replay()   rebuilds the FTS memtable from WAL entries
         └─ doc_count = segments + memtable
```

## Wire compatibility and conformance

Elasticsearch compatibility is verified by the `es-yaml-runner` harness against the
ES 8.13 REST-API-spec YAML suites (search, aggregations, vectors, bulk, indices,
scroll, cluster). The hard gate is zero failed cases; skipped cases are reported
separately and totals can change as the upstream suite changes. The YAML tests are
the source of truth: if XERJ returns a different response than a test expects, XERJ
is considered wrong. See the README's "Running the conformance tests" section for
how to run the suites and the full list of supported query types and aggregations.

Performance is tracked with a reproducible full-matrix head-to-head against live
Elasticsearch 8.13.4, published at <https://xerj.org/benchmarks> (per-cell results
in `demo/playbooks/FULL_MATRIX_SCORECARD_*.md`). The scorecard is deliberately
honest about both wins and losses — numbers are only published after an
independent adversarial re-measure.

## Where to read more

- [`AGENTS.md`](../AGENTS.md) — agent/reviewer guide: positioning, ground rules,
  and the authoritative Architecture Overview (sharded ingest, WAL, search flow).
- `engine/releases/v0.1.0/reports/` — dated engineering reports, including
  `BENCHMARK_VS_ES_2026-06-30_phase2.md` (benchmark methodology and results) and the
  `ES_YAML_PROGRESS_*` conformance progress reports.
- Source entry points worth reading first: `engine/crates/xerj-api/src/es_compat.rs`
  (REST surface), `engine/crates/xerj-query/src/parser.rs` (ES JSON → AST), and
  `engine/crates/xerj-engine/src/index.rs` (`Index::search` and `flush`).
