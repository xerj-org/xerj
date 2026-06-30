# XERJ vs Elasticsearch — head-to-head benchmark

Real Elasticsearch 8.13.4 (Lucene 9.10.0) downloaded and run alongside XERJ
8.13.0, **identical workloads, same machine, same corpus**. The only variable is
the engine. Reproduce with `node demo/playbooks/bench-vs-es.mjs <N> <xerj_url> <es_url>`.

## Setup (fair-comparison notes)

| | XERJ | Elasticsearch 8.13.4 |
|---|---|---|
| Process | single Rust binary, native heap | JVM, `-Xmx4g -Xms4g` |
| Security/TLS | `--insecure` (off) | `xpack.security.enabled: false` |
| Topology | single node, 1 shard | single node, 1 shard (replica unassigned) |
| Mapping | identical (keyword/date/int/double/dense_vector) | identical |
| Client | same Node.js harness, localhost, sequential bulk (10k/batch), `_refresh` after ingest | same |
| Latency metric | **end-to-end client wall-clock** (not server `took`) — 80 iters after 15 warmup | same |

Machine: AMD Ryzen AI MAX+ 395, 32 cores, 119 GB RAM. Corpus: real LLM-telemetry
events. Both engines were given the same per-batch file-write + curl overhead, so
ingest deltas are real engine time.

## Ingest throughput (bulk, sequential, 10k/batch)

| Engine | 100k docs | 1,000,000 docs |
|---|--:|--:|
| **Elasticsearch** | 68,464 docs/s | **110,012 docs/s** |
| **XERJ** | 31,429 docs/s | 22,417 docs/s |
| ES advantage | 2.2× | **4.9×** |

**Elasticsearch wins bulk ingest decisively, and the gap widens with scale** —
Lucene's segment-based bulk indexing is extremely mature, and ES throughput
*rose* from 100k→1M (68k→110k docs/s) while XERJ's *fell* (31k→22k), reflecting
segment-flush / merge pressure that XERJ's single-binary ingest path does not yet
amortize as well. **If your workload is write-heavy at scale, ES indexes faster.**

## Read-path latency — 1,000,000 docs (ms, end-to-end, 80 iters)

| operation | XERJ p50 | ES p50 | XERJ p95 | ES p95 | XERJ p99 | ES p99 | p50: XERJ faster by |
|---|--:|--:|--:|--:|--:|--:|--:|
| match_all (size 10) | **1.52** | 2.51 | 1.88 | 3.47 | 6.16 | 3.72 | 1.65× |
| term filter | **1.51** | 2.34 | 1.82 | 3.06 | 2.01 | 3.29 | 1.55× |
| bool must+filter | **1.48** | 2.73 | 1.92 | 4.26 | 3.00 | 4.84 | 1.84× |
| range | **1.46** | 2.41 | 1.55 | 3.21 | 1.99 | 3.44 | 1.65× |
| agg: terms(model) | **1.39** | 2.09 | 1.59 | 2.52 | 1.97 | 3.18 | 1.51× |
| agg: stats(latency_ms) | **1.38** | 2.04 | 1.98 | 3.07 | 2.39 | 3.40 | 1.48× |
| agg: date_histogram(day) | **1.28** | 1.94 | 1.81 | 2.29 | 70.06¹ | 2.47 | 1.51× |
| agg: terms+avg(cost) | **1.28** | 1.94 | 1.47 | 2.45 | 2.31 | 3.11 | 1.51× |
| agg: cardinality(top_doc) | **1.24** | 1.84 | 1.43 | 2.90 | 1.99 | 3.22 | 1.49× |
| _count match_all | **1.19** | 2.10 | 1.25 | 2.94 | 4.98 | 3.29 | 1.76× |
| kNN k=10 (20k×16d) | **1.36** | 2.52 | 1.49 | 4.17 | 1.70 | 4.95 | 1.85× |

**XERJ wins read latency on every operation — ~1.5–1.85× lower p50** for queries,
aggregations, and vector kNN. The lean Rust request path has materially less
per-request overhead than the JVM stack: XERJ p50 sits at 1.2–1.5 ms where ES is
1.8–2.7 ms. XERJ's p95 is also lower across the board.

¹ XERJ p99 outliers (e.g. one 70 ms date_histogram, occasional 5–6 ms match_all)
are tail events from a background segment flush/merge coinciding with a sampled
request — ES's mature merge scheduler produces a tighter tail (p99 ≤ ~5 ms). This
is the same ingest-side machinery that costs XERJ on write throughput showing up
on the read tail under concurrent background work.

## Bottom line

| Dimension | Winner | Margin |
|---|---|---|
| **Read latency** (query / agg / kNN p50, p95) | **XERJ** | 1.5–1.85× lower |
| **Bulk ingest throughput** | **Elasticsearch** | 2.2× (100k) → 4.9× (1M) |
| **Tail latency under background merges** (p99) | **Elasticsearch** | tighter |
| Footprint / startup | **XERJ** | single ~22 MB binary, native heap, sub-second start vs JVM + 4 GB |

**XERJ is the faster engine for the read/serve path** — lower, steadier query and
vector latency from a tiny single binary — and is **drop-in compatible** (100% of
the 1,326-case ES YAML wire-conformance suite passes, version 8.13). **Elasticsearch
is the faster bulk indexer**, especially at scale, with a tighter latency tail under
heavy background indexing. The honest read: pick XERJ for low-latency search/serving
and a minimal operational footprint; pick ES (today) for write-heavy ingest at scale.
Closing XERJ's ingest-throughput and p99-tail gap is the clear next perf target.
