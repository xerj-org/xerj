# xerj

**AI-native search, vector, and log engine. Rust/C. Single binary. Zero GC. Byte-perfect.**

xerj is an Elasticsearch-compatible search engine that replaces ES clusters with a single binary.

## Quick Start

```bash
# Download and run
./xerj --data-dir ./data --insecure

# Or with Docker
docker run -v xerj-data:/data -p 9200:9200 xerj --insecure

# Create an index
curl -X PUT http://localhost:9200/my-index

# Index a document
curl -X PUT http://localhost:9200/my-index/_doc/1 \
  -H 'Content-Type: application/json' \
  -d '{"title": "Hello", "content": "World"}'

# Search
curl -X POST http://localhost:9200/my-index/_search \
  -H 'Content-Type: application/json' \
  -d '{"query": {"match": {"content": "world"}}}'
```

## Why xerj?

Head-to-head against Elasticsearch 8.13.0 on the same machine, identical
HTTP wire payloads, single-node, tmpfs data dir, both freshly started
([full report][bench]):

| | Elasticsearch 8.13 | xerj | Winner |
|---|---:|---:|---|
| Binary size | ~800 MB | ~16 MB | **xerj 50×** |
| Cold start (after restart) | 6.04 s | **0.40 s** | **xerj 15×** |
| Graceful shutdown (SIGTERM) | 3.27 s | **0.24 s** | **xerj 14×** |
| RSS, warmed (100k docs + 1k vectors) | 2,527 MB | **86 MB** | **xerj 29×** |
| Disk, post-restart | 6 MB | **4 MB** | **xerj 1.5×** |
| Per-shard WAL post-flush | 55 B | **16 B** | **xerj 3.4×** |
| Index creation (`PUT /idx`) | 23.5 ms | **2.6 ms** | **xerj 9×** |
| `PUT /_doc/{id}?refresh=true` p50 | 6.50 ms | **0.34 ms** | **xerj 19×** |
| `DELETE /_doc/{id}?refresh=true` p50 | 5.20 ms | **0.30 ms** | **xerj 17×** |
| Term query p50 | 0.79 ms | **0.32 ms** | **xerj 2.5×** |
| Match query p50 | 1.28 ms | **0.35 ms** | **xerj 3.7×** |
| Terms agg p50 | 1.06 ms | **0.33 ms** | **xerj 3.2×** |
| Top-K sort p50 | 4.50 ms | **0.35 ms** | **xerj 13×** |
| kNN k=10 p50 | 1.43 ms | **0.49 ms** | **xerj 2.9×** |
| Bulk-ingest 100k docs/s | **179,574** | 95,405 | ES 1.88× |
| Per-batch (5K) bulk p99 | **34.8 ms** | 80.3 ms | ES 2.3× |
| Data loss on restart | 0% | 0% | tied |
| GC pauses | 20–40 ms (JVM) | **0** (no GC) | **xerj** |
| Config surface | 3,000+ knobs | **38** | **xerj** |

xerj wins on every operational and read-path metric. ES still wins
bulk-ingest throughput on warm Lucene `IndexWriter` (perf-backlog item
to close — sharded flush thresholds + doc-values borrow refactor).

[bench]: reports/2026-04-25T13-40-00_xerj_vs_elasticsearch_disk_parity.md

## Features

- **100% ES API compatible** — drop-in replacement, existing clients work unchanged
- **BM25 full-text search** with highlighting, aggregations, and all ES query types
- **Vector search** with HNSW, filtered ANN, and inline embedding
- **SQL API** — query with SQL syntax
- **WAL persistence** — crash recovery, data survives restarts
- **Security by default** — auth + TLS on by default
- **Zero dependencies** — single static binary, no JVM, no Lucene

## Building

```bash
cargo build --release -p xerj-server
```

The resulting binary is at `target/release/xerj`.

## Configuration

See [`xerj.default.toml`](xerj.default.toml) for all 38 settings with documentation.

The minimal config to get started:

```toml
[server]
data_dir = "/var/lib/xerj"
es_compat_port = 9200

[auth]
enabled = false   # set true and provide admin_api_key in production
```

Pass a config file with `--config`:

```bash
xerj --config /etc/xerj/xerj.toml
```

## ES API Coverage

### Index & Document APIs

| Operation | Endpoint |
|---|---|
| Create index | `PUT /{index}` |
| Delete index | `DELETE /{index}` |
| Index document | `PUT /{index}/_doc/{id}`, `POST /{index}/_doc` |
| Get document | `GET /{index}/_doc/{id}` |
| Delete document | `DELETE /{index}/_doc/{id}` |
| Update document | `POST /{index}/_update/{id}` |
| Bulk API | `POST /_bulk` |
| Delete by query | `POST /{index}/_delete_by_query` |
| Refresh | `POST /{index}/_refresh` |
| Flush | `POST /{index}/_flush` |
| Get mapping | `GET /{index}/_mapping` |
| Put mapping | `PUT /{index}/_mapping` |
| Index stats | `GET /{index}/_stats` |

### Search APIs

| Operation | Endpoint |
|---|---|
| Search | `POST /{index}/_search` |
| Multi-search | `POST /_msearch` |
| Scroll | `POST /{index}/_search?scroll=1m`, `POST /_search/scroll` |
| Search template | `POST /{index}/_search/template` |
| Async search | `POST /{index}/_async_search` |
| Count | `POST /{index}/_count` |

### Cluster & Management APIs

| Operation | Endpoint |
|---|---|
| Cluster health | `GET /_cluster/health` |
| Node info | `GET /_nodes` |
| Index aliases | `POST /_aliases`, `GET /_alias` |
| Index templates | `PUT /_index_template/{name}` |
| Data streams | `PUT /_data_stream/{name}` |
| ILM policies | `PUT /_ilm/policy/{name}` |
| Snapshot repos | `PUT /_snapshot/{repo}` |
| SQL query | `POST /_sql` |

## Query Types Supported

| Query Type | Notes |
|---|---|
| `match_all` / `match_none` | Universal matchers |
| `match` | Full-text BM25 search |
| `match_phrase` | Ordered phrase search |
| `match_phrase_prefix` | Autocomplete-style |
| `multi_match` | Search across multiple fields |
| `term` / `terms` | Exact keyword match |
| `range` | Numeric, date, keyword ranges |
| `prefix` | Prefix match on keyword fields |
| `wildcard` | `*` and `?` glob patterns |
| `regexp` | Full regex on keyword fields |
| `fuzzy` | Edit-distance fuzzy match |
| `exists` | Field presence check |
| `ids` | Match by document ID list |
| `bool` | `must`, `should`, `must_not`, `filter` |
| `query_string` | Lucene query syntax |
| `simple_query_string` | Simplified query syntax |
| `constant_score` | Wrap any query with a fixed score |
| `boosting` | Positive/negative boosting |
| `dis_max` | Disjunction max across queries |
| `geo_distance` | Filter by radius from a geo point |
| `knn` | k-Nearest neighbour vector search |
| `semantic` | Semantic search via embedding proxy |
| `hybrid` | Combine BM25 + vector scores |

## Aggregation Types Supported

| Aggregation | Type |
|---|---|
| `terms` | Bucket — group by field value |
| `range` | Bucket — group by numeric/date ranges |
| `histogram` | Bucket — fixed-width numeric intervals |
| `date_histogram` | Bucket — calendar-aware time buckets |
| `filter` | Bucket — apply a query as a filter |
| `missing` | Bucket — documents missing a field |
| `composite` | Bucket — paginate across combinations |
| `avg` / `sum` / `min` / `max` | Metric — basic statistics |
| `stats` | Metric — all basic statistics in one pass |
| `value_count` | Metric — count of non-null values |
| `cardinality` | Metric — approximate distinct count |
| `percentiles` | Metric — p50, p95, p99, etc. |

## Architecture Overview

```
HTTP Request
    → xerj-api  (Axum handler, es_compat.rs)
    → xerj-query  parse_request() — raw JSON → SearchRequest (QueryNode tree)
    → Engine::get_index() — looks up named index
    → Index::search()
         → memtable scan  (in-memory BM25 via FtsMemtable)
         → segment scan   (on-disk FTS via FtsIndexReader + BM25)
         → doc_matches_query() for term-level / geo queries
         → run_aggs() for aggregations
         → apply_source_filter(), apply_highlight()
    → SearchResult → JSON response
```

### Crate Map

| Crate | Purpose |
|---|---|
| `xerj-server` | Binary entry point; CLI, config loading, server startup |
| `xerj-api` | Axum HTTP layer; ES-compatible and native REST handlers |
| `xerj-engine` | `Engine` and `Index` structs — the integration layer |
| `xerj-query` | Query DSL: AST, ES JSON parser, planner, rewriter, executor |
| `xerj-storage` | WAL, segments, version map, index store |
| `xerj-fts` | BM25 scoring, analyzer registry, postings lists |
| `xerj-common` | Shared types: `Config`, `Schema`, `FieldType`, `XerjError` |
| `xerj-vector` | Dense vector HNSW index for k-NN / semantic search |
| `xerj-compress` | Block compression codecs (LZ4, Zstd) |
| `xerj-logs` | Columnar log ingestion and time-based retention |
| `xerj-ai` | Text chunking, embedding proxy, memory store |

### Storage Model

Each index is stored as a directory under `data_dir/`:

```
data/
  my-index/
    wal/          WAL segments (crash-safe write buffer)
    seg-000001/   Immutable on-disk segment (after first flush)
    seg-000002/
    schema.json   Field mapping definition
```

Documents are written to the WAL and an in-memory memtable first. The memtable is flushed to a durable segment when it exceeds `flush_size_mb` (default 256 MiB) or `flush_interval_secs` (default 30 s). Segments are periodically merged in the background.

## Security

By default, xerj requires an API key on every request. On first startup, the admin key is auto-generated and written to `<data_dir>/admin.key`:

```bash
curl -H "Authorization: ApiKey <key>" http://localhost:9200/_cluster/health
```

Use `--insecure` to disable auth and TLS in development environments.

## Pricing

xerj is open source. No ERU licensing. No per-node fees. No feature gates.

All capabilities — vector search, full-text search, aggregations, SQL, log ingestion,
embedding proxy — are available in the single open-source binary with no paid tiers.

## Aggregation Exactness

xerj `terms` aggregations are **exact**, not estimates.

Elasticsearch uses the HyperLogLog++ algorithm for `cardinality` and can return
approximate bucket counts for `terms` aggregations under high cardinality.
xerj computes every aggregation over the full document set with precise counts —
there is no sampling, no approximation, and no `doc_count_error_upper_bound`.

## Vector Dimensions

xerj supports up to **16,384 dimensions** per vector field — 4× Elasticsearch's
limit of 4,096.  This accommodates current and next-generation embedding models:

| Model                   | Dimensions |
|-------------------------|------------|
| MiniLM / all-MiniLM-L6  | 384        |
| BERT-base               | 768        |
| OpenAI ada-002          | 1,536      |
| OpenAI text-embedding-3 | 3,072      |
| GPT-4o embeddings       | 3,072      |
| Custom / future models  | ≤ 16,384   |

Set `max_dimensions` in `xerj.toml` (default is 16,384).

## Embedding Token Limits

xerj delegates embedding generation to the configured external model API
(e.g. OpenAI, Ollama, or a self-hosted model).  Token limits are therefore
**model-specific**, not xerj-specific:

- OpenAI `text-embedding-ada-002` — 8,191 tokens per input
- OpenAI `text-embedding-3-*`     — 8,191 tokens per input
- Ollama `nomic-embed-text`       — model-dependent (typically 2,048–8,192)

xerj's chunker (`xerj-ai`) splits long documents into model-appropriate
windows before calling the embedding API.  Configure the endpoint and model
in `[embedding]` in your config file.

## Denormalization / Update by Query

`POST /{index}/_update_by_query` performs actual field updates on matching
documents.  This is the recommended pattern for denormalization cascades:

```bash
# Update all orders where customer.id = "C123" to reflect a name change
curl -X POST http://localhost:9200/orders/_update_by_query \
  -H 'Content-Type: application/json' \
  -d '{
    "query": { "term": { "customer.id": "C123" } },
    "script": { "source": "ctx._source.customer.name = params.name",
                "params": { "name": "Acme Corp" } }
  }'
```

## SIEM Use Cases

xerj is purpose-built as a data substrate for SIEM (Security Information and
Event Management) deployments:

- **Log ingestion** — syslog (RFC 3164/5424), OTLP, and structured JSON ingest
  via `POST /{index}/syslog` and `POST /{index}/otlp`
- **Time-series retention** — configurable per-index TTL with automatic deletion
- **Full-text + vector search** — correlate logs with threat intelligence embeddings
- **Aggregations** — exact `terms` and `date_histogram` for alert thresholds
- **Single binary** — deploy on-prem or in isolated networks with no external deps

## Contributing

1. Fork the repository and create a feature branch.
2. Run `cargo check` and `cargo test` before opening a PR.
3. Keep commits focused; one logical change per commit.
4. All public APIs must have doc-comments.

```bash
# Check all crates compile
cargo check

# Run tests
cargo test

# Run a specific crate's tests
cargo test -p xerj-engine

# Lint
cargo clippy -- -D warnings
```

## License

Apache 2.0 — see [LICENSE](LICENSE).
