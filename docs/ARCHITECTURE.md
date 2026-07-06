# XERJ Architecture

This document orients contributors to how XERJ is put together: the crate layout,
the path a search request takes, and the path an indexed document takes. It is a
map, not a specification — the source is authoritative, and
[`engine/CLAUDE.md`](../engine/CLAUDE.md) is the canonical quick reference for
build/run/test commands and the supported ES surface.

XERJ is an Elasticsearch-wire-compatible search, vector, and log-analytics engine
written in Rust and published under Apache-2.0. It speaks the Elasticsearch 8.x
HTTP protocol, so existing ES clients, dashboards, and ingest tooling can talk to
it unchanged.

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
| `xerj-vector` | Dense-vector HNSW index for k-NN / semantic search. |
| `xerj-logs` | Columnar log ingestion and retention. |
| `xerj-ai` | Text chunking, embedding proxy, memory store. |
| `xerj-compress` | Block compression codecs (LZ4, Zstd). |
| `xerj-common` | Shared types: `Config`, `Schema`, `FieldType`, `XerjError`. |
| `xerj-cluster` | Embedded Raft consensus for cluster metadata (no external dependencies). |
| `xerj-console-api` | Bundled console backend (dashboards, auth, prefs) mounted at `/_xerj-console/api/v1/*`. |
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
         ├─ memtable scan        in-memory BM25 via FtsMemtable
         ├─ segment scan         on-disk FTS via FtsIndexReader + BM25
         ├─ doc_matches_query()  term-level / geo predicate evaluation
         ├─ run_aggs()           aggregation pipeline (columnar fast path for size:0)
         └─ apply_source_filter(), apply_highlight()
    → SearchResult → JSON response
```

A query is matched against **both** the in-memory memtable and the on-disk segments,
and the results are merged so that freshly written documents are immediately
searchable. For `size > 0` requests, hit materialization is bounded to the top
`from + size` candidates. k-NN and hybrid queries fan the vector portion out to the
HNSW index in `xerj-vector` and combine scores in the same executor.

## The ingest path

Writes go through a write-ahead log into a sharded memtable, then flush to immutable
segments that are later merged:

```
PUT /{index}/_doc/{id}   or   POST /_bulk
    → xerj-api → xerj-engine → xerj-storage (IndexStore)
         ├─ WAL append          single Mutex<WalWriter>; monotonic seq_no, one fsync per batch
         ├─ 16-shard memtable   shard = xxh3_64(doc_id) & 15  (memtable_shards: Vec<Mutex<Vec<MemEntry>>>)
         ├─ flush               take_memtable_for_flush() drains all shards,
         │                      sorts by WAL seq_no to preserve global order,
         │                      writes an immutable segment (LZ4/Zstd blocks)
         └─ merge               background segment merge compacts small segments;
                                _forcemerge is synchronous + quiescent (ES-like)
```

The WAL writer is a single mutex so that sequence numbers are globally monotonic;
lock hold time is kept short (one batched write and fsync). The storage memtable is
sharded 16 ways to spread write contention. The engine-side FTS memtable is
currently `Arc<RwLock<FtsMemtable>>`; a `ShardedFtsMemtable` scaffold exists in
`memtable.rs` for a future refactor.

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
scroll, cluster). XERJ currently passes 1,326 of 1,329 cases. The YAML tests are the
source of truth: if XERJ returns a different response than a test expects, XERJ is
considered wrong. See `engine/CLAUDE.md` for how to run the suites and the full list
of supported query types and aggregations.

Performance is tracked with a reproducible 91-cell head-to-head matrix against
Elasticsearch 8.13, published at <https://xerj.org/benchmarks>. The scorecard is
deliberately honest about both wins and losses.

## Where to read more

- [`engine/CLAUDE.md`](../engine/CLAUDE.md) — build/run/test commands, crate table,
  and the authoritative Architecture Overview (sharded ingest, WAL, search flow).
- `engine/releases/v0.1.0/reports/` — dated engineering reports, including
  `BENCHMARK_VS_ES_2026-06-30_phase2.md` (benchmark methodology and results) and the
  `ES_YAML_PROGRESS_*` conformance progress reports.
- Source entry points worth reading first: `engine/crates/xerj-api/src/es_compat.rs`
  (REST surface), `engine/crates/xerj-query/src/parser.rs` (ES JSON → AST), and
  `engine/crates/xerj-engine/src/index.rs` (`Index::search` and `flush`).
