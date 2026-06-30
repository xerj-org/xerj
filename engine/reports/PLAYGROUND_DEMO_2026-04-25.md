# Xerj Console — the Xerj playground UI

**Status (v0.7.0):** shipped, embedded in every xerj binary,
served at `http://localhost:9200/_xerj-console/`. 36 files / ~520 KiB
of HTML+CSS+JS bundled at compile time via
`engine/crates/xerj-api/build.rs`.

Xerj Console is a **typography-first observability dashboard** that
runs against the Xerj engine today and is architected so a
later release can swap in Kibana / OpenSearch / vanilla ES as
the data backend without touching dashboard code. The UI gets
its own product name (Xerj Console) so the brief / website can talk
about it independently from the engine (Xerj).

---

## 1. Demo path — get to the UI in three commands

```bash
# 1. Download a 0.7.0 binary (or use the one you just built).
curl -L -o xerj.tar.gz \
  https://github.com/xerj-ai/xerj/releases/download/v0.7.0/xerj-0.7.0-x86_64-unknown-linux-gnu.tar.gz
tar xzf xerj.tar.gz && cd xerj-0.7.0-x86_64-unknown-linux-gnu

# 2. Start the engine.
./xerj --insecure --data-dir ./data
```

```text
  ██████╗ ███████╗███████╗██████╗ ██████╗ 
 ╚════██╗██╔════╝██╔════╝██╔══██╗██╔══██╗
  █████╔╝█████╗  █████╗  ██████╔╝██████╔╝
  ╚═══██╗██╔══╝  ██╔══╝  ██╔══██╗██╔══██╗
 ██████╔╝███████╗███████╗██████╔╝██████╔╝ 
 ╚═════╝ ╚══════╝╚══════╝╚═════╝ ╚═════╝  v0.7.0

 byte-perfect from design to code

 Native REST  :8080 [plain]
 ES-compat    :9200 [plain]
 gRPC         :8081 [placeholder]
 Data dir     ./data
 Started in   3ms

 Xerj Console UI    http://localhost:9200/_xerj-console/  (36 files bundled)
```

```bash
# 3. Open the UI.
open http://localhost:9200/_xerj-console/
```

That's it. No Kibana to install, no docker-compose to memorise,
no `xpack.security.enabled=false` — one binary, one URL.

---

## 2. What "typography-first" means

The Xerj Console surface is one nav strip + one canvas, both rendered
in `Big Shoulders Display` for headings and `JetBrains Mono` for
data. **Every value is visible without hover**: there are no
charts that demand a tooltip to read, no donut that you can't
sort, no card with a gradient background that costs you 200 ms
of paint cycles for no information gain.

```text
┌─ XERJ CONSOLE ──────────────────────────────────────────────────────── LIVE · Xerj · http://localhost:9200 ─┐
│ OBSERVE   SEARCH   AI   DATA   USERS   SETTINGS                                                       │
├───────────────────────────────────────────────────────────────────────────────────────────────────────┤
│  LOGS · OVERVIEW · LAST 24H                                                                           │
│                                                                                                       │
│  TOTAL EVENTS         BY LEVEL                       TIMELINE                                         │
│  ──────────────       ─────────────────              ──────────────────────────────────────────       │
│  1,284,991            INFO   ████████████  912k       12k ▏▎▍▌▋▊▉█▉▊▋▌▍▎▏▏▎▍▌▋▊▉█▉▊▋▌▍▎ rate         │
│                       WARN   ██▍           186k        8k                                             │
│  +12% vs prev 24h     ERROR  █▎             97k        4k                                             │
│                       DEBUG  ▏              89k        0  00 03 06 09 12 15 18 21                    │
└───────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

The numbers above are mock; the layout is what every dashboard
follows. Once xerj holds real logs, the same tile reads from
`POST /logs-*/_search` with a `date_histogram` agg.

---

## 3. The four dashboards wired live to Xerj today

### 3.1 SEARCH · DISCOVER

The interactive query console. Type a natural-language phrase or
a structured `field:value` query; pick from 8 query types
(`match`, `term`, `range`, `prefix`, `phrase`, `knn`, `semantic`,
`hybrid`); see the actual ES-compat request body alongside the
hits.

**What the UI sends to Xerj:**
```json
POST /logs-prod/_search
{
  "query": { "match": { "message": "request" } },
  "size": 25,
  "track_total_hits": true,
  "aggs": {
    "by_level":   { "terms": { "field": "level",   "size": 8 } },
    "by_service": { "terms": { "field": "service", "size": 8 } },
    "by_host":    { "terms": { "field": "host",    "size": 8 } }
  }
}
```

**What Xerj Console renders:**

```text
SEARCH · DISCOVER · index: logs-prod · type: match
┌──────────────────────────────────────────────────────────────────────────────────────────────┐
│ ▶ message:request                                                                  [SEARCH]  │
└──────────────────────────────────────────────────────────────────────────────────────────────┘

HITS · 8 in 0.7 ms                                  FACETS
─────────────────────────                           ───────────────────
_id   _score  level   message                       LEVEL
d1    1.04    INFO    request 1 processed in 12ms     INFO   ████ 4
d3    1.04    ERROR   request 3 processed in 36ms     WARN   ███  3
d5    1.04    INFO    request 5 processed in 60ms     ERROR  ██   1
d7    1.04    INFO    request 7 processed in 84ms                              
                                                    HOST
                                                      web01  ███  3
                                                      web02  ███  3
                                                      web03  ██   2
QUERY DSL                                           PLAN
─────────────────────────                           ───────────────────
{ match: { message: "request" } }                   BoolQuery (cost=8)
                                                    └ MatchQuery field=message, value=request
```

**Hybrid query** flips the type-picker to `hybrid` and sends:

```json
POST /logs-prod/_search
{
  "query": {
    "hybrid": {
      "queries": [
        { "query": { "match": { "message": "error" } }, "weight": 1.0 },
        { "query": { "semantic": { "field": "embedding", "query": "error", "k": 10 } },
          "weight": 0.8 }
      ],
      "fusion": { "type": "rrf", "k": 60 }
    }
  },
  "size": 25
}
```

xerj v0.7.0 ships the executor for both `hybrid` and `semantic`
— the same query that crashed in v0.6.x now returns RRF-fused
hits in one round trip. The engine plumbing is in
`xerj-engine/src/index.rs::run_hybrid` /
`run_semantic`; the UI surface is in
`playground/src/data/backends/xerj.js::buildSearchBody`.

---

### 3.2 SYSTEM

Cluster health, node count, total docs, index inventory.
Single tile, sourced from three concurrent calls:

```bash
GET /_cluster/health
GET /_nodes/stats
GET /_cat/indices?format=json
```

**Live response from a fresh xerj:**
```json
{
  "cluster_name": "xerj",
  "status": "green",
  "active_primary_shards": 1,
  "number_of_nodes": 1,
  "number_of_data_nodes": 1
}
```

**Xerj Console surface:**

```text
SYSTEM · cluster=xerj · NODE 1 of 1
─────────────────────────────────────────────────────────────────
STATUS         green
NODES          1
INDICES        1
DOCS TOTAL     8
PRIMARY SH.    1
ACTIVE %       100.0%
SHARDS RELOC.  0
SHARDS INIT.   0

INDEX             HEALTH  DOCS    SIZE     STATUS
────────────────  ──────  ─────   ──────   ──────
logs-prod         green   8       3.4kb    open
```

---

### 3.3 LOGS · OVERVIEW

Time-series tile + per-level breakdown. The UI sends one
`date_histogram` request to xerj and the response feeds both
the timeline and the per-level facet.

```json
POST /logs-*/_search
{
  "size": 0,
  "query": { "range": { "@timestamp": { "gte": "now-24h" } } },
  "aggs": {
    "by_level": { "terms": { "field": "level", "size": 5 } },
    "timeline": {
      "date_histogram": { "field": "@timestamp", "fixed_interval": "1h" },
      "aggs": { "by_level": { "terms": { "field": "level", "size": 5 } } }
    }
  }
}
```

The bucket-size (`1m` for 1 H, `1h` for 24 H, `6h` for 7 D, `1d`
for 30 D / 90 D) is picked client-side from the time-range
control, so the timeline always renders ~24 columns regardless
of range.

---

### 3.4 DATA

Index inventory: name, health, docs, size, shard count. Powered
by `_cat/indices?format=json` — one HTTP call, zero client-side
work.

```text
DATA · INDICES
─────────────────────────────────────────────────────────
NAME           HEALTH  DOCS         SIZE      STATUS
logs-prod      green   8            3.4kb     open
ai-kb          green   40           18.2kb    open
```

---

## 4. The other ten dashboards — fall back to mock today

| Dashboard          | Live adapter? | Mock OK? | Roadmap milestone     |
|--------------------|--------------:|---------:|-----------------------|
| ai-overview        | no            | yes      | v0.7.x — agg over `_search?aggs=…` |
| agent-memory       | no            | yes      | v0.7   — wires to persistent `xerj-ai::memory_store` |
| vector-index       | no            | yes      | v0.7   — `_admin/segments/fsck` + HNSW stats |
| rag-quality        | no            | yes      | v0.8   — needs reranker hook |
| ingest-pipeline    | no            | yes      | v0.8   — once pipeline processors execute |
| anomaly-detect     | no            | yes      | v1.x   — out of scope until ML lands |
| alerts             | no            | yes      | v1.x   — pairs with Watcher work |
| users              | no            | yes      | v0.9   — RBAC milestone |
| settings           | partial       | yes      | v0.9   — Config diff against runtime |

The status pill in the nav makes the difference visible at all
times: `LIVE · Xerj · …` for live, `Xerj: MOCK FALLBACK` for
a configured-but-unreachable backend, `MOCK DATA` if the user
explicitly switches to the mock backend in settings.

---

## 5. Architecture — backend abstraction

Every dashboard reads from a single `query()` function in
`playground/src/data/query.js`. `query()` looks up the active
backend (`xerj` by default, persisted in localStorage) and
dispatches to its `search(baseUrl, dashId, ctx, signal)` method.

```text
       Dashboard render(data, meta)
                │
                ▼
        data/query.js   ← one entry point, always
                │
                ▼
    activeBackend()  →  src/data/backends/xerj.js     (live HTTP)
                                       /mock.js        (in-memory)
                                       /…              (kibana, vanilla-es later)
```

A new backend module needs three functions:

```js
export const meta = { id, label, defaultBaseUrl, supports: {…} };
export async function probe(baseUrl, signal) { … }       // is this backend live?
export async function search(baseUrl, dashId, ctx, signal) { … }  // dashboard data
```

— and a one-line registration in `backends/index.js`. No
dashboard touches HTTP directly; switching from Xerj to
OpenSearch will be a settings flip, not a code change.

---

## 6. What this commit set ships

| Commit  | Subject |
|---------|---------|
| `9c5ed70` | engine — bundle Xerj Console into the binary, mount at /_xerj-console on both ports |
| `a867e6c` | playground — rename to Xerj Console + backend abstraction (Xerj live + Mock fallback) |

Verified:

* `target/release/xerj --insecure` prints
  `Xerj Console UI    http://localhost:9200/_xerj-console/  (36 files bundled)`.
* `curl http://localhost:9200/_xerj-console/` returns the playground
  index HTML.
* `curl http://localhost:9200/_xerj-console/src/data/query.js` returns
  the wired query gateway.
* The bundled-asset count is 36 files / ~520 KiB, embedded via
  `include_bytes!` from `build.rs`.
* `strings target/release/xerj | grep "Xerj Console · typography"`
  matches — proof the bundle is in the binary, not loose on disk.

---

## 7. v0.7.x demo asks (what to add next)

1. **Live `vector-index` dashboard** — show `tombstone_count`,
   graph-node count, p99 kNN latency. Powered by the v0.6.2
   `_admin/segments/fsck` plus a new lightweight stats endpoint.
2. **Live `agent-memory`** — once the persistent memory store
   from v0.7-P1 is wired, the dashboard reads its segments and
   shows recall-by-recency curves.
3. **Backend picker in Settings** — UI to switch between Xerj,
   ES, OpenSearch, Kibana with a baseURL field per backend. The
   abstraction is in place; the picker is one settings panel.
4. **One-screen "Hybrid lab"** — type a query, see BM25 hits,
   semantic hits, and RRF-fused hits side-by-side with the
   per-rank contributions. Demos the v0.7 hybrid executor in a
   way that tells the story better than 1000 words of brief.

Tracked under the v0.7.x section of
`PATH_TO_100_PCT_v0.6.0_to_v1.0.md`.

---

*Compiled 2026-04-25. Update each time a dashboard moves from
mock to live, or a new backend module lands.*
