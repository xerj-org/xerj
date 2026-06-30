# xerj Engine — Claude Code Guide

xerj is an Elasticsearch-compatible search engine written in Rust.
This document is the quick-reference for Claude Code working in this repository.

## Git workflow (mandatory since 2026-04-15)

Every non-trivial change lands on a task-named branch, gets a commit
with a full body explaining motivation, benchmark numbers, or bug
root-cause, and then fast-forwards into `main`.  The git history
is the session log — future Claude sessions read it to understand
what was attempted, why, and what moved the needle.

```bash
# New work
cd /home/claude/ai/xerj   # repo root   # git root (one level above engine/)
git checkout main && git pull
git checkout -b <type>/<short-slug>   # e.g. perf/shard-wal, fix/merge-hang

# ...edit code...
cargo build --release -p xerj-server

# Commit with body
git add engine/... playground/... user-feedback/...
git commit  # detailed body in $EDITOR, or -m '"$(cat <<EOF ... EOF)"'

# Merge + push
git checkout main
git merge --ff-only <branch>
git push origin main
git push origin <branch>   # keep the branch for history
```

**Commit body MUST include**: motivation, what changed, before/after
benchmark numbers (if perf), known trade-offs, and the file pointers
a future session needs.  Example body for a perf commit:

```
perf(xerj-engine): parallel Index::flush() across shards

Pre: user-visible flush() walked shards serially in a for-loop,
so the end-of-file flush in `xerj index` CLI took 10-15s on a
2 GB memtable, dragging 20M-doc benchmarks from 1.0M docs/s peak
down to 480k docs/s total.

Fix: spawn one tokio task per shard, flush_sema-bounded, join all.
Each shard drains into its own segment, no cross-shard dependency.

After: 5-run median 944 k docs/s fully segment-durable, variance
reduced from 2.2× to 1.15×.

Files:
- crates/xerj-engine/src/index.rs::flush (line ~3185)

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>
```

**Do not force-push to main.**  Never `git commit --amend` on a commit
that's already been pushed.  Always create fresh commits for fixes.

## Build

```bash
# Build the server binary (release mode)
cargo build --release -p xerj-server

# Build all crates
cargo build --release
```

## Run

```bash
# Start the server with a local data directory (insecure = no TLS, no auth)
target/release/xerj --data-dir ./data --insecure

# With a custom config file
target/release/xerj --config xerj.default.toml --data-dir ./data
```

Default listen address: `http://0.0.0.0:9200`

## Test

```bash
# Run all unit + integration tests
cargo test

# Run only the xerj-engine integration tests
cargo test -p xerj-engine --test integration

# Run a specific test by name
cargo test -p xerj-engine test_wal_persistence

# Run with output visible
cargo test -- --nocapture
```

## ES-Compat YAML Tests (mandatory — 100% pass target)

**Status as of 2026-04-16 16:54 UTC:** Runner built and verified.
1,329 test cases parsed from 182 YAML files extracted from ES 8.13
source. Tests NOT yet run against a live XERJ instance — awaiting
first full pass. **Every ES REST API test must pass with NO EXCUSE.**
Failures are bugs in XERJ, not acceptable divergences.

The YAML tests are the source of truth for ES wire compatibility.
If XERJ returns a different response than what the test expects,
XERJ is wrong.

```bash
# Start XERJ in test mode
target/release/xerj --insecure --data-dir /tmp/xerj-test

# Run ALL ES-compat tests (1,329 cases)
cargo run -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml --verbose

# Run one suite at a time
cargo run -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml/search
cargo run -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml/aggregations
cargo run -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml/bulk
cargo run -p es-yaml-runner -- --dir tests/es-compat-yaml/yaml/vectors

# Run a single file
cargo run -p es-yaml-runner -- --file tests/es-compat-yaml/yaml/bulk/10_basic.yml --verbose
```

### Test suites and counts

| Suite | Files | Cases | Validates |
|---|---|---|---|
| search | 60 | ~400 | All 38 query types |
| aggregations | 78 | ~500 | All agg types (terms, date_histogram, percentiles, composite) |
| vectors | 53 | ~300 | kNN, dense_vector, hybrid search |
| bulk | 10 | ~60 | Bulk indexing semantics |
| indices | 39 | ~200 | Create/delete/alias/template/mapping |
| scroll | 4 | ~30 | Scroll API pagination |
| cluster | 1 | ~5 | Cluster health |
| smoke | 1 | ~5 | Multinode smoke |
| **Total** | **182** | **~1,329** | |

### Updating tests from ES source

```bash
# Pull fresh ES source and re-copy (runner reads whatever YAML is in the dir)
cp -r es-reference/rest-api-spec/.../test/search engine/tests/es-compat-yaml/yaml/
cp -r es-reference/modules/aggregations/.../test/aggregations engine/tests/es-compat-yaml/yaml/
# No code changes needed — the runner discovers files at runtime.
```

### Rules

1. **100% pass rate is the target.** No "known failures" list. No
   "intentional divergences." If the test says `match: { hits.total.value: 5 }`
   and XERJ returns 4, that's a bug. Fix it.
2. **Run the full suite before every release.** Regressions caught by
   a YAML test that was passing yesterday are P0 bugs.
3. **When adding a new ES-compat feature**, find the matching YAML test
   file and confirm it passes. If no YAML test exists, write one in the
   same format and add it to `tests/es-compat-yaml/yaml/`.
4. **The runner is at `tests/es-compat-yaml/`.** It's a workspace member.
   Build with `cargo build -p es-yaml-runner`. Source: `src/main.rs` (~400 LOC).

## Crate Structure

| Crate | Purpose |
|---|---|
| `xerj-server` | Binary entry-point; CLI argument parsing, config loading, starts the API |
| `xerj-api` | Axum HTTP layer; ES-compatible REST handlers (`es_compat.rs`) and native API (`native.rs`) |
| `xerj-engine` | Integration crate — the `Engine` and `Index` structs that tie everything together |
| `xerj-query` | Query DSL: AST (`ast.rs`), ES JSON parser (`parser.rs`), query planner, rewriter, executor |
| `xerj-storage` | WAL, segments, version map, index store |
| `xerj-fts` | Full-text search: BM25 scoring, analyzer registry, postings lists |
| `xerj-common` | Shared types: `Config`, `Schema`, `FieldType`, `XerjError` |
| `xerj-vector` | Dense vector HNSW index for k-NN / semantic search |
| `xerj-compress` | Block compression codecs (LZ4, Zstd) |
| `xerj-logs` | Columnar log ingestion and retention |
| `xerj-ai` | Text chunking, embedding proxy, memory store |

## ES API Compatibility Notes

- **Index / document APIs** — `PUT /{index}/_doc/{id}`, `GET /{index}/_doc/{id}`, `DELETE /{index}/_doc/{id}`, `POST /{index}/_update/{id}` are supported.
- **Search** — `POST /{index}/_search` accepts a standard ES request body with `query`, `from`, `size`, `sort`, `aggs`, `_source`, `highlight`.
- **Supported query types** — `match_all`, `match_none`, `match`, `match_phrase`, `multi_match`, `term`, `terms`, `range`, `prefix`, `wildcard`, `exists`, `ids`, `bool`, `fuzzy`, `regexp`, `query_string`, `match_phrase_prefix`, `simple_query_string`, `constant_score`, `boosting`, `dis_max`, `geo_distance`, `knn`, `semantic`, `hybrid`.
- **Supported aggregations** — `terms`, `stats`, `avg`, `sum`, `min`, `max`, `value_count`, `cardinality`, `range`, `histogram`, `date_histogram`, `percentiles`, `filter`, `missing`, `composite`.
- **Bulk API** — `POST /_bulk` with `index`, `create`, `update`, `delete` actions.
- **Scroll API** — `POST /{index}/_search?scroll=1m`, `POST /_search/scroll`.
- **Delete by query** — `POST /{index}/_delete_by_query`.
- **Index templates** — `PUT /_index_template/{name}`.
- **Aliases** — `POST /_aliases` with `add`/`remove` actions.

## Release Artifacts

All reports, benchmarks, battle results, architecture docs, and
roadmaps live under `engine/releases/v0.1.0/reports/` (43 files).
New dated report files go there — never spam `engine/` root.

Key files:
- `RELEASE_NOTES.md` — index of all artifacts
- `ROADMAP_TO_100_PCT.md` — phased plan to reach 100% ES YAML test pass rate
- `ES_YAML_TEST_RESULTS_2026-04-16_171500_UTC.md` — first full test run (198/1329 = 14.9%)
- `ES_vs_XERJ_2026-04-16_053023_UTC.md` — full ES 8.13 vs XERJ feature comparison

## Architecture Overview

For the ingest path (WAL, memtable, sharded routing) the authoritative
reference is **`releases/v0.1.0/reports/ARCHITECTURE_V5_SHARDED_INGEST_2026-04-15.md`**.  In short:

* **Storage memtable is 16-shard**: `IndexStore.memtable_shards:
  Vec<Mutex<Vec<MemEntry>>>`.  `wal_append_batch` routes each doc to
  `shard = xxh3_64(doc_id) & 15`.  `take_memtable_for_flush` drains
  all shards and sorts by WAL `seq_no` to preserve global order.
* **WAL writer is a single `Mutex<WalWriter>`** — seq_no generation is
  monotonic.  Hold time is kept short (per-batch write, one fsync).
* **Engine FTS memtable is still `Arc<RwLock<FtsMemtable>>`** — a
  `ShardedFtsMemtable` scaffold exists in `memtable.rs` and is ready
  to wire once the `try_aggs_fast` doc-values borrow is refactored
  (see V5 §2.4 and §6).

For the v3-era broader architecture, see `ARCHITECTURE_V4_2026-04-14.md`.

Brief flow for a search request:

Brief flow for a search request:

```
HTTP Request
    → xerj-api (Axum handler, es_compat.rs)
    → xerj-query parse_request() — raw JSON → SearchRequest (QueryNode tree)
    → Engine::get_index() — looks up named index
    → Index::search()
         → memtable scan (in-memory BM25 via FtsMemtable)
         → segment scan (on-disk FTS via FtsIndexReader + BM25)
         → doc_matches_query() for term-level / geo queries
         → run_aggs() for aggregations
         → apply_source_filter(), apply_highlight()
    → SearchResult → JSON response
```

WAL replay on restart:

```
Engine::new() scans data_dir/
    → for each index dir: Index::open()
        → IndexStore::open() replays WAL into storage memtable
        → WalReader::replay() rebuilds FTS memtable from WAL entries
        → doc_count = segments + memtable
```
