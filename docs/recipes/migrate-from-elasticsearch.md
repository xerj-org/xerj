# Migrate from Elasticsearch: change the URL, not your app

**Use case:** You have a service that talks to Elasticsearch — it creates indices with mappings, bulk-loads documents, runs `match`/`term`/`range`/`bool` searches, computes `terms` aggregations, and checks `_cat/indices` in ops dashboards. You want to move to XERJ without rewriting any of that.

**The whole migration is one line:** point your client's base URL at XERJ. XERJ speaks the Elasticsearch REST wire protocol on the same `:9200`, so the exact JSON your app already sends works unchanged. No SDK swap, no query rewrites, no re-mapping.

```diff
- ES_URL = "http://elasticsearch:9200"
+ ES_URL = "http://xerj:9200"
```

This recipe proves it by running the standard ES request shapes — verbatim — against a live XERJ and showing the real responses.

## Why XERJ for this

- **Same wire, same client.** The official `elasticsearch-py`, `@elastic/elasticsearch`, or a raw `curl` all work because XERJ implements the ES REST API surface. XERJ even reports itself as version `8.13.0` at `GET /`, so version-sniffing clients connect cleanly.
- **Faster on the same workload.** On an identical corpus and machine, XERJ beats a real Elasticsearch 8.13 on both ingest and reads — e.g. bulk ingest ~1.5–1.7× and `match` queries ~2.5× lower p50 latency (see [Benchmarks](#benchmarks)).
- **Honest scope.** XERJ is *wire-compatible*, not a byte-for-byte fork. As of the last full run it passes **1,325 / 1,326** executed cases of the 1,329-case ES REST conformance suite (3 skipped, 1 known fail). Response *shapes* match; some internal details (exact BM25 float scores, segment-merge timing) are XERJ's own. See [Limitations](#limitations-be-honest).

## The working solution

Everything below is a plain `curl` in the exact shape you'd send Elasticsearch. The only variable is `$ES`.

### 1. Create an index with a mapping — `PUT /products`

```bash
curl -s -X PUT "$ES/products" -H 'Content-Type: application/json' -d '{
  "mappings": {
    "properties": {
      "name":     { "type": "text" },
      "brand":    { "type": "keyword" },
      "price":    { "type": "float" },
      "in_stock": { "type": "boolean" }
    }
  }
}'
```

Response — the standard ES acknowledgement:

```json
{ "acknowledged": true, "shards_acknowledged": true, "index": "products" }
```

### 2. Bulk-load documents — `POST /_bulk`

Standard NDJSON: an action line, then a source line, repeated. `Content-Type: application/x-ndjson`, trailing newline.

```bash
curl -s -X POST "$ES/_bulk" -H 'Content-Type: application/x-ndjson' --data-binary '
{"index":{"_index":"products","_id":"1"}}
{"name":"Aluminum water bottle","brand":"Klean","price":24.99,"in_stock":true}
{"index":{"_index":"products","_id":"2"}}
{"name":"Insulated steel water bottle","brand":"Hydro","price":39.95,"in_stock":true}
... 4 more ...
'
```

Response is the ES bulk envelope (`{"took":..,"errors":false,"items":[...]}`) — here, `errors: false, items: 6`.

> As in Elasticsearch, freshly indexed docs become searchable after a refresh. Call `POST /products/_refresh` (or wait for the interval) before the search steps.

### 3. Full-text `match` — `POST /products/_search`

```bash
curl -s "$ES/products/_search" -H 'Content-Type: application/json' -d '{
  "query": { "match": { "name": "water bottle" } },
  "_source": ["name","brand","price"]
}'
```

Real result — BM25-ranked, standard `hits.total.value` / `_score` shape:

```
total: 4
  0.575  Aluminum water bottle
  0.575  Insulated steel water bottle
  0.288  Glass water carafe          # matched "water"
  0.288  Plastic sports bottle       # matched "bottle"
```

Note the classic OR semantics of `match`: "Glass water carafe" and "Plastic sports bottle" each match one term and rank below the docs matching both. The full hit shape is exactly ES's:

```json
{
  "hits": {
    "total": { "value": 4, "relation": "eq" },
    "max_score": 1.4168,
    "hits": [
      { "_index": "products", "_id": "1", "_score": 1.4168,
        "_source": { "name": "Aluminum water bottle", "brand": "Klean", "price": 24.99 } }
    ]
  }
}
```

### 4. The classic `bool` combo — `must` + `terms` filter + `range` filter

The bread-and-butter Elasticsearch query: full-text relevance in `must`, non-scoring constraints in `filter`.

```bash
curl -s "$ES/products/_search" -H 'Content-Type: application/json' -d '{
  "query": {
    "bool": {
      "must":   [ { "match": { "name": "bottle" } } ],
      "filter": [
        { "terms": { "brand": ["Hydro","Klean"] } },
        { "range": { "price": { "gte": 10, "lte": 40 } } }
      ]
    }
  },
  "_source": ["name","brand","price"]
}'
```

Real result — only the docs that match the text **and** pass both filters:

```
total: 2
  Klean   24.99  Aluminum water bottle
  Hydro   39.95  Insulated steel water bottle
```

(The Klean "Plastic sports bottle" is excluded — its price `9.99` is below the `10` floor.)

### 5. `terms` aggregation — `POST /products/_search` with `size: 0`

```bash
curl -s "$ES/products/_search" -H 'Content-Type: application/json' -d '{
  "size": 0,
  "aggs": { "by_brand": { "terms": { "field": "brand" } } }
}'
```

Real result — the standard `buckets` shape with `doc_count_error_upper_bound` and `sum_other_doc_count`:

```json
{
  "hits": { "total": { "value": 6, "relation": "eq" }, "hits": [] },
  "aggregations": {
    "by_brand": {
      "doc_count_error_upper_bound": 0,
      "sum_other_doc_count": 0,
      "buckets": [
        { "key": "Bodum", "doc_count": 2 },
        { "key": "Hydro", "doc_count": 2 },
        { "key": "Klean", "doc_count": 2 }
      ]
    }
  }
}
```

### 6. Ops check — `GET /_cat/indices?v`

The human-readable `_cat` table your dashboards and shell scripts parse:

```
health status index    uuid       pri rep docs.count docs.deleted store.size pri.store.size
green  open   products  f5c15d1... 1   0   6          0            10901b     10901b
```

`_cat/indices` also lists XERJ's internal `.xerj_*` system indices (sessions, users, dashboards, etc.), analogous to Elasticsearch's own `.security`/`.kibana` dot-indices. Filter them the same way you would in ES: `GET /_cat/indices/products?v` or a `products*` pattern.

## Run it

The full script is [`docs/examples/migrate-from-elasticsearch/migrate_demo.sh`](../examples/migrate-from-elasticsearch/migrate_demo.sh). Boot XERJ and run every step above end-to-end:

```bash
# 1. Boot XERJ (insecure = no TLS/auth, for local demo)
xerj --insecure --data-dir /tmp/xerj-demo &

# 2. Run the standard-ES demo against it — the ONLY change is the URL
ES=http://localhost:9200 ./migrate_demo.sh
```

It prints each request's result and ends with `All standard Elasticsearch calls succeeded against XERJ`. To A/B against a real cluster, run the identical script with `ES=http://your-elasticsearch:9200` — same script, same output shape.

## What to change in your real app

Almost nothing:

1. **Base URL** → your XERJ host. That's the migration.
2. **Auth/TLS** → XERJ supports them; the demo uses `--insecure` for brevity. Point your client's existing credentials/CA config at XERJ as you would any ES endpoint.
3. **Client library** → keep it. `elasticsearch-py`, the JS client, Logstash/Beats outputs, or raw HTTP all speak this wire.

## Limitations (be honest)

- **Wire-compatible, not byte-identical.** XERJ passes **1,325 / 1,326** executed cases of the 1,329-case ES REST conformance suite (3 skipped, 1 known fail), last full run. Response *structure* matches ES; validate the specific endpoints and query types your app depends on against your own data before cutting over. Per-suite conformance and caveats are tracked in `demo/playbooks/ES_COMPATIBILITY.md`.
- **Relevance scores are XERJ's own BM25.** Ranking order matches ES intent, but exact `_score` float values differ, and can even shift slightly for the *same* query as background segment merges change collection statistics — the same class of internal difference you'd see comparing two ES minor versions. If you assert on exact scores in tests, relax those assertions.
- **A few query/agg types differ or aren't implemented.** XERJ covers the common set (`match`, `term`, `terms`, `range`, `bool`, `match_phrase`, `multi_match`, `terms`/`stats`/`date_histogram`/... aggs). Some ES features are partial or unsupported (e.g. vector-suite conformance is low). Check the compatibility report for your specific needs.

## Benchmarks

Same corpus, same machine, single node, security off — XERJ vs. a real Elasticsearch 8.13:

| Operation | XERJ | ES 8.13 | XERJ advantage |
|---|---|---|---|
| ingest 1M docs, 1 client (docs/s) | 119,031 | 70,320 | **1.69×** |
| ingest 100k docs, 8 clients (docs/s) | 382,301 | 253,961 | **1.51×** |
| `match(model)` read (p50 ms) | 0.57 | 1.45 | **2.53×** |
| `match_all` read (p50 ms) | 0.88 | 1.72 | **1.94×** |
| `query_string` read (p50 ms) | 1.35 | 9.35 | **6.92×** |

Full head-to-head matrix and methodology (identical keep-alive HTTP client applied to both engines): `demo/playbooks/BENCHMARK_VS_ES.md`.

---

*Verified end-to-end against a live XERJ (merged `main` binary, ES-wire port). Every request/response above is real captured output.*
