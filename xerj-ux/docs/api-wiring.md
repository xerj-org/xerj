# Playground → API wiring plan

Goal: replace the playground's mock data layer with real calls to the
current Elasticsearch-compatible API, with the minimum amount of new
backend work.

The playground already renders 13 dashboards from chart primitives in
`playground/src/ux/charts*.js`. Most panels can be served by a plain
`POST /{index}/_search` with an aggregation body — the same request
Kibana would send. Only the AI/vector dashboards need new native
endpoints; everything else can go live with zero backend changes.

---

## 1. API inventory (current state)

### Native API (port `8080`)

| Method | Path | Handler | What it returns |
|---|---|---|---|
| POST | `/v1/indices` | `native::create_index` | Create index + schema |
| GET | `/v1/indices/:name` | `native::get_index` | Index metadata |
| DELETE | `/v1/indices/:name` | `native::delete_index` | Delete index |
| POST | `/v1/indices/:name/docs` | `native::ingest_docs` | Single/batch ingest |
| GET | `/v1/indices/:name/docs/:id` | `native::get_doc` | Doc by id |
| DELETE | `/v1/indices/:name/docs/:id` | `native::delete_doc` | Tombstone a doc |
| POST | `/v1/indices/:name/docs/_bulk` | `native::bulk_ingest` | Bulk ingest (NDJSON) |
| POST | `/v1/indices/:name/turbo-ingest` | `native::turbo_ingest` | High-throughput ingest |
| GET | `/v1/indices/:name/encodings` | `native::get_index_encodings` | Per-field encoding stats |
| POST | `/v1/indices/:name/search` | `native::search` | Full-text + aggs (native body) |
| POST | `/v1/indices/:name/_flush` | `native::flush_index` | Force memtable flush |
| GET | `/v1/health` | `native::health` | Liveness |
| GET | `/v1/metrics` | `native::metrics` | Prometheus text |
| GET | `/v1/dashboard/summary` | `native::dashboard_summary` | Index list + doc/byte counts |

### ES-compat API (port `9200`) — the one we wire against

| Method | Path | Returns |
|---|---|---|
| GET | `/` | Cluster info (ES wire) |
| GET | `/_cluster/health` | Cluster health snapshot (JSON) |
| GET | `/_cat/indices` | Index list (CSV) |
| GET | `/_cat/health` | Node health (CSV) |
| GET/POST | `/:index/_search` | **Query + aggs (ES DSL) — workhorse** |
| GET/POST | `/:index/_count` | Doc count for a query |
| GET | `/:index/_stats` | Size, doc count, segment count |
| GET | `/:index/_mapping` | Field mapping |
| GET | `/:index/_settings` | Index settings |
| POST | `/:index/_bulk` | Bulk ops |
| POST | `/_bulk` | Global bulk |

The vast majority of dashboard panels can be served by
`POST /:index/_search` with different aggregation bodies.

---

## 2. Dashboard inventory

| ID | File | Panels (chart primitives used) | Mock source |
|---|---|---|---|
| `ai-overview` | `ai-overview.js` | `Num`, `Spark`, `Series`, `Dist`, `Ribbon3D`, `FlowBand`, `TopN`, `Citations` | `mock.js::buildAiOverview()` |
| `rag-quality` | `rag-quality.js` | `Num`, `Spark`, `Series`, `Dist`, `Heatmap`, `ChordArcs`, `AttentionMap`, `TopN` | `mock.js::buildRagQuality()` |
| `vector-index` | `vector-index.js` | `Num`, `Series`, `Multiples`, `EmbedSpace`, `ParallelCoords`, `Ribbon3D`, `Gauge`, `TopN` | `mock.js::buildVectorIndex()` |
| `agent-memory` | `agent-memory.js` | `Num`, `Series`, `Dist`, `EmbedSpace`, `Table`, `TopN` | `mock.js::buildAgentMemory()` |
| `anomaly-detect` | `anomaly-detect.js` | `Num`, `Spark`, `Dist`, `AnomalyBand`, `Treemap`, `ParallelCoords`, `AttentionMap`, `TopN` | `mock.js::buildAnomalyDetect()` |
| `ingest-pipeline` | `ingest-pipeline.js` | `Num`, `Series`, `Multiples`, `FlowBand`, `MetricTile`, `Gauge`, `Table`, `TopN` | `mock.js::buildIngestPipeline()` |
| `logs-overview` | `logs-overview.js` | `Num`, `Series`, `Dist`, `Heatmap`, `TopN` | `mock.js::buildLogsOverview()` |
| `system` | `system.js` | `Num`, `Series`, `Multiples`, `TopN` | `mock.js::buildSystem()` |
| `search-discover` | `search-discover.js` | `SearchBox`, `Series`, `Hits`, `Facet`, `QueryDSL`, `QueryPlanTree` | **Already live** — `buildDsl()` → `/v1/indices/:name/search` |
| `alerts` | `alerts.js` | `Num`, `Series`, `Dist`, `TopN`, `Events`, `Table`, `FlowBand` | Hardcoded in `render()` |
| `data` | `data.js` | Metadata tables | `data-sources.js::listIndices`/`listFields` |
| `users` | `users.js` | Auth metadata | Stubs |
| `settings` | `settings.js` | Config UI | App state |

Chart primitives take these input shapes (keep these stable — no
refactor needed, adapters translate API responses into these):

- `Num(value, unit, spark, delta)`
- `Series(values[], {h, labels, unit})` — `values` is `number[]`
- `Spark(values[], {w, h})`
- `Dist({segments, width})` — `segments = [{label, value}]`
- `Heatmap({rows, cols, matrix})` — `matrix = number[][]`
- `TopN({items, total, n})` — `items = [{label, value}]`
- `Multiples({items})` — `items = [{label, values, value}]`
- `EmbedSpace({clusters, h})` — `clusters = [{label, points, centroid}]`
- `Ribbon3D({series, h, depth})` — `series = [{label, values}]`
- `FlowBand({segments, unit})` — `segments = [{label, value}]`
- `ParallelCoords({dims, rows, highlight, h})`
- `Gauge({value, min, max})`
- `ChordArcs({sources, targets, flows})` — `flows = [{from, to, weight}]`

---

## 3. Mapping — dashboard panel → ES query

Group by dashboard so each one can be wired in a single sitting.

### `logs-overview` (fully ES-compat — do this first)

**Total events + sparkline**
```json
POST /logs/_search
{ "query": {"match_all": {}},
  "aggs": { "by_time": { "date_histogram":
    {"field": "@timestamp", "fixed_interval": "15m"} } },
  "size": 0 }
```
Response: `aggregations.by_time.buckets[*].doc_count` →
`Num(sum, 'events', values[])` and `Series(values)`.

**By level**
```json
POST /logs/_search
{ "aggs": { "by_level":
    { "terms": {"field": "level", "size": 10} } },
  "size": 0 }
```
Response: `aggregations.by_level.buckets[*]` →
`Dist({segments: buckets.map(b => ({label: b.key, value: b.doc_count}))})`.

**Top services**
```json
POST /logs/_search
{ "aggs": { "top_services":
    { "terms": {"field": "service", "size": 10} } },
  "size": 0 }
```
Response: `aggregations.top_services.buckets[*]` → `TopN({items})`.

**Weekday × 2h heatmap**
```json
POST /logs/_search
{ "aggs": { "by_dow":
    { "terms": {"field": "day_of_week", "size": 7},
      "aggs": { "by_hour":
        { "date_histogram":
          {"field":"@timestamp","fixed_interval":"2h"} } } } },
  "size": 0 }
```
Response: nested `aggregations.by_dow.buckets[*].by_hour.buckets[*]` →
pivot into `matrix[day][hour_bucket]` for `Heatmap({rows, cols, matrix})`.

### `system`

**CPU / memory / disk over time**
```json
POST /metrics/_search
{ "query": { "terms":
    {"metric_name": ["system.cpu","system.memory"] } },
  "aggs": { "by_time":
    { "date_histogram": {"field":"@timestamp","fixed_interval":"5m"},
      "aggs": { "v": {"avg": {"field":"value"}} } } },
  "size": 0 }
```
Response: `aggregations.by_time.buckets[*].v.value` → `Series(values)`.

**Per-host CPU (small multiples)**
```json
POST /metrics/_search
{ "query": { "term": {"metric_name":"system.cpu"} },
  "aggs": { "by_host":
    { "terms": {"field":"host","size":20},
      "aggs": { "cpu":
        { "date_histogram": {"field":"@timestamp","fixed_interval":"5m"},
          "aggs": { "v": {"avg":{"field":"value"}} } } } } },
  "size": 0 }
```
Response → one `Series` per host, bundled into `Multiples({items})`.

### `ai-overview`

**LLM queries over time + total**
```json
POST /llm-events/_search
{ "aggs": { "by_time":
  { "date_histogram":
    {"field":"@timestamp","fixed_interval":"30m"} } },
  "size": 0 }
```

**By model**
```json
POST /llm-events/_search
{ "aggs": { "by_model":
  { "terms": {"field":"model","size":10} } },
  "size": 0 }
```

**Total tokens + cost**
```json
POST /llm-events/_search
{ "aggs": {
    "tokens": { "sum": {"field": "tokens_total"} },
    "by_model_tokens": {
      "terms": {"field":"model","size":10},
      "aggs": { "toks": {"sum":{"field":"tokens_total"}} }
    }
  },
  "size": 0 }
```
Cost comes from multiplying tokens × a client-side pricing table
(no endpoint needed; pricing lives in `playground/src/data/pricing.js`).

**Token budget breakdown** (`FlowBand`)
```json
POST /llm-events/_search
{ "aggs": {
    "sys": {"sum":{"field":"tokens_system"}},
    "ctx": {"sum":{"field":"tokens_context"}},
    "q":   {"sum":{"field":"tokens_question"}},
    "ans": {"sum":{"field":"tokens_completion"}}
  },
  "size": 0 }
```
Response: four scalars → `FlowBand({segments: [{label:'SYS', value:sys}, ...]})`.

### `anomaly-detect`

Anomalies = z-score over a `date_histogram`. Fetch the raw series
with the same query as `logs-overview::total events`, compute
mean/stddev **client-side**, mark any bucket > 3σ. No new endpoint.

### `ingest-pipeline`

Most panels map to native `/v1/metrics` (Prometheus text). Instead,
parse `GET /v1/dashboard/summary` for per-index rates and
`/_cluster/health` for node count. Throughput sparklines need a
metrics index — same `metrics` index pattern as `system`.

### `alerts` · `agent-memory` · `data` · `users` · `settings`

- `alerts`: wire `Events({items})` via `POST /alerts/_search` sorted
  by `@timestamp` desc — plain hits list, no aggs.
- `agent-memory`: one `_search` with a `knn` clause hitting a
  `memory_vec` field. Already works via ES-compat (`knn` is in the
  query DSL).
- `data`: already uses `data-sources.js` which hits the native
  `/v1/indices` and `/v1/indices/:name` — keep as-is.
- `users`: app-local; no API needed.
- `settings`: app-local; no API needed.

---

## 4. Gaps

| Panel / dashboard | Why it doesn't fit ES-compat | Severity | Cheapest fix |
|---|---|---|---|
| `EmbedSpace` (2D cluster hulls — `vector-index`, `agent-memory`, `rag-quality`) | No ES agg produces 2D projections. | **blocker** for the vector dashboards | New native `POST /v1/indices/:name/_project` that runs UMAP/t-SNE server-side, OR fetch raw vectors and UMAP client-side via a JS lib. For MVP: client-side. |
| `Gauge` for HNSW recall@10 (`vector-index`) | Recall is engine-internal, not a document field. | blocker | New native `GET /v1/indices/:name/_vector_stats` returning `{recall_at_10, build_latency_p95, vectors_indexed}`. Tiny endpoint. |
| `Ribbon3D` for latency distribution (`ai-overview`, `anomaly-detect`) | 3D rendering is client-side; data is p50/p95/p99 over time. | nice-to-have | Standard `percentiles` agg gives the numbers; ribbon is a pure chart concern. |
| `AnomalyBand` confidence band | ES doesn't compute the band. | nice-to-have | Compute ±2σ client-side from the same `date_histogram` the panel already pulls. |
| `AttentionMap` (`rag-quality`) | Per-token attention weights are LLM-internal. | blocker until such data is indexed | Out of scope — keep mock until an `llm_attention` index exists. |
| `Treemap` (`anomaly-detect`) | Requires hierarchical bucketing. | nice-to-have | Composite terms agg gives leaf buckets; the hierarchy is client-side derived from `host.service.path` strings. |
| `ChordArcs` service graph (`rag-quality`, any "service flows") | Graph topology is not an agg. | nice-to-have | Two pivoted terms aggs (`source_service` × `target_service` with `doc_count`) → client-side converts to `{from, to, weight}`. Works today. |
| `Citations` (user feedback across all dashboards) | Stored in a JS file, not an index. | none | Leave as a local JSON file. |
| Cluster / node stats (`system`) | `/v1/metrics` is Prometheus text, not JSON. | nice-to-have | Parse the Prom text in the playground — small parser (~30 lines), or add a tiny native `GET /v1/cluster/stats` JSON endpoint. |

Blockers are only in the vector / AI-attention space. The other ~60%
of dashboards can be wired against the current ES-compat API
**without touching the engine**.

---

## 5. Wiring strategy — the easiest path

1. **One API module.** Create `playground/src/data/api.js` with
   two functions and nothing else:

   ```js
   export async function esSearch(index, body) {
     const base = localStorage.getItem('xerj.apiUrl') || 'http://localhost:9200';
     const key  = localStorage.getItem('xerj.apiKey') || '';
     const res = await fetch(`${base}/${index}/_search`, {
       method: 'POST',
       headers: {
         'content-type': 'application/json',
         ...(key ? {'authorization': `ApiKey ${key}`} : {}),
       },
       body: JSON.stringify(body),
     });
     if (!res.ok) throw new Error(`search failed: ${res.status}`);
     return res.json();
   }
   export async function clusterHealth() { /* GET /_cluster/health */ }
   ```

2. **Adapters sit in `playground/src/data/adapters.js`** — one tiny
   function per dashboard panel that takes the raw `_search` response
   and returns the shape the chart primitive expects
   (`{segments}` / `{items}` / `values[]` / `matrix[][]`). These
   replace the mock generator functions one at a time.

3. **Wire dashboards in this order**, biggest-unlock-first:
   1. **`logs-overview`** — 5 panels, 4 queries, zero gaps. Proves
      the whole stack end-to-end.
   2. **`system`** — same query shapes (histogram + terms), different
      index. Unlocks the `Multiples` primitive against real data.
   3. **`ai-overview`** — same again, plus the token-budget FlowBand.
   4. **`anomaly-detect`** — reuses `logs-overview` queries, adds
      client-side z-score.
   5. **`search-discover`** — already live; just expose its
      `buildDsl()` body to the other dashboards for consistency.
   6. **`ingest-pipeline` / `alerts`** — drop-in after the `metrics`
      index is populated.
   7. **Vector dashboards** (`vector-index`, `agent-memory`,
      `rag-quality`) — wait for the two new native endpoints
      (`_project` + `_vector_stats`). Keep mock until then.

4. **Auth**: single API key in `localStorage` under `xerj.apiKey`,
   set from the Settings dashboard. `api.js` reads it on every call.
   No per-dashboard config.

5. **Index picker**: the top-level index dropdown already exists in
   `chrome.js`. Persist the current choice in `localStorage` under
   `xerj.index` and read it from each dashboard instead of
   hardcoding index names.

6. **Error handling**: on fetch failure, fall back to the existing
   mock generator with a corner badge ("MOCK · NO BACKEND"). One-line
   try/catch wrapper in every adapter. Dashboards keep rendering.

7. **Zero chart primitive changes.** All existing `Series`,
   `Heatmap`, `TopN`, etc. keep their current signatures. Only the
   data layer moves.

8. **Seeding data locally**:
   ```sh
   # From a repo-local script (doesn't need to exist yet)
   curl -X POST http://localhost:9200/logs/_bulk \
     -H 'content-type: application/x-ndjson' \
     --data-binary @fixtures/logs-24h.ndjson
   ```
   Write a tiny fixture generator that emits ~100k NDJSON lines of
   realistic logs, metrics, and LLM events into three indices.
   One night, done.

9. **Milestone check**: after step 3(3), 60% of panels across the
   site are on live data. The remaining 40% are the vector/AI
   panels that need new endpoints — split into a follow-up.

Expected effort: **2–3 days** to ship steps 1–5 (ES-compat
dashboards). **1 day of backend work** for `_vector_stats` +
`_project` to unlock the rest.
