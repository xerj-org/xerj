# Query and Search Performance Issues

## Severity: HIGH | Frequency: MODERATE

---

## Core Complaints

### Deep Pagination Memory Cliff
- ES does NOT skip to page N; fetches and ranks ALL results up to offset, then discards
- Every shard does this work independently
- Hard cap at 10,000 results (`index.max_result_window`)
- Page 500 causes timeouts while page 1 loads in milliseconds
- `search_after` and scroll APIs are workarounds that add state management complexity

### Version Upgrade Regressions
- 7.4 → 7.10: performance regression (GitHub #67574)
- 5.6 → 7.16: performance regression (GitHub #89972)
- 8.6 → 8.16: performance regression (GitHub #118623)
- CPU spikes, higher flush rates, degraded cluster performance

### Refresh/Indexing Trade-Off
- Default 1-second refresh creates new Lucene segment every second
- Consumes heavy I/O and CPU
- Increasing interval to 30s improves indexing 2-3x but data invisible to search during that window
- Near real-time, NOT real-time: documents not searchable until next refresh

### Merge Storms
- New segments created faster than background merge can consolidate
- ES throttles indexing to single thread when merging can't keep up
- Causes intermittent throughput drops and unpredictable latency

### Global Ordinals Rebuild
- After each refresh, first query on high-cardinality keyword fields experiences massive latency spike
- ES rebuilds in-memory ordinal structures synchronously

### Slow Query Diagnosis
- Requires enabling slow logs and search profiling (both add overhead)
- Without them, identifying latency cause is guesswork
- Common culprits: regex queries, leading-wildcard queries, heavy aggregations, deeply nested aggs

### Scripting Without Limits
- A script that never halts permanently occupies a search thread pool slot
- Repeated executions exhaust pool, blocking ALL searches cluster-wide

---

## User Quotes

> "When there is a large text search, the elastic search sometime crashes"
> -- Anis B., Software Development Engineer, 10,001+ employees (Capterra)

> "Performance during peak period not stable enough"
> -- Julie Zhong, Data Analytics, Netnordic (TrustRadius)

> "Really easy to make your performance really low, you have to be careful"
> -- Backend Developer, IT (Capterra)

> "Encountered scaling challenges with large datasets (petabytes)"
> -- TrustRadius reviewer

---

## XERJ.ai Response
- Cursor-based pagination (no deep pagination cliff)
- Unified segment format = simpler merge, predictable I/O
- rayon work-stealing parallelism = no thread pool exhaustion
- No JVM-related refresh overhead
- Query timeout enforcement at the engine level
- Cost-based query planner prevents degenerate execution plans

## Sources
- GitHub Issues: #67574, #89972, #118623
- Elastic Blog: Finding Slow Queries
- Coralogix: Query Performance
- Elastic Blog: Six Ways to Crash
