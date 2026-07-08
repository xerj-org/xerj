# Log analytics with XERJ: from raw logs to dashboards

**Goal:** You have a stream of structured application logs — timestamp, level,
service, latency, status — and you need to answer the questions an on-call
engineer actually asks: *Is the error rate climbing? Which service owns the
latency tail? Who's taking the most traffic?* This recipe bulk-ingests a few
hundred synthetic log events and answers all three with plain Elasticsearch
aggregations against a single XERJ instance — no separate metrics store, no
second query language.

Everything below was run end-to-end against a live XERJ. The numbers and JSON
are the real captured output.

## Why XERJ for this

XERJ speaks the Elasticsearch wire protocol, so the aggregation surface you
already know — `date_histogram`, `percentiles`, `terms`, `stats`, and `filter`
sub-aggs — works as-is. You ingest with `_bulk` and query with `_search`. One
Rust engine handles the logs, the aggregations, and (if you want them later)
full-text and vector search on the same documents. No Logstash/metrics-store
split to operate.

## Set up the index

Give the log fields explicit types up front. `@timestamp` must be a `date` for
`date_histogram`, `latency_ms`/`status` numeric for math, and `service`/`level`
`keyword` so they bucket exactly instead of being analyzed.

```bash
curl -XPUT localhost:9200/logs-app -H 'Content-Type: application/json' -d '{
  "mappings": { "properties": {
    "@timestamp": {"type": "date"},
    "level":      {"type": "keyword"},
    "service":    {"type": "keyword"},
    "latency_ms": {"type": "integer"},
    "status":     {"type": "integer"},
    "message":    {"type": "text"}
  }}
}'
```

## Ingest with `_bulk`

Logs arrive in volume, so use the bulk API: alternating action + document
NDJSON lines. The example script generates 600 events across 6 hours and 4
services (`checkout`, `search`, `catalog`, `payments`), each with its own
baseline latency and error propensity so the aggregations tell a real story.
One `_bulk` call ships them all:

```
POST /_bulk           (Content-Type: application/x-ndjson)
{"index":{"_index":"logs-app"}}
{"@timestamp":"2026-07-06T08:14:03+00:00","level":"INFO","service":"search","latency_ms":61,"status":200,"message":"search handled request status=200"}
{"index":{"_index":"logs-app"}}
{"@timestamp":"2026-07-06T08:22:41+00:00","level":"ERROR","service":"payments","latency_ms":740,"status":503,"message":"payments handled request status=503"}
...
```

Check `errors` in the response is `false`, then `_refresh` to make the docs
searchable. `GET /logs-app/_count` confirms 600 landed.

## Q1 — Error rate over time

Bucket events by hour with `date_histogram`, and inside each bucket count the
ERROR events with a `filter` sub-agg. `errors.doc_count / doc_count` is the
error rate for that hour — exactly the shape a dashboard panel needs.

```json
POST /logs-app/_search
{
  "size": 0,
  "aggs": {
    "per_hour": {
      "date_histogram": { "field": "@timestamp", "fixed_interval": "1h" },
      "aggs": {
        "errors": { "filter": { "term": { "level": "ERROR" } } }
      }
    }
  }
}
```

`size: 0` means "no hits, just the aggregations." Real response (first two
buckets, `took` in ms):

```json
{
  "took": 4,
  "buckets": [
    { "key_as_string": "2026-07-06T08:00:00.000Z", "doc_count": 102,
      "errors": { "doc_count": 7 } },
    { "key_as_string": "2026-07-06T09:00:00.000Z", "doc_count": 101,
      "errors": { "doc_count": 3 } }
  ]
}
```

Rolled up across all six buckets:

```
hour (UTC)            total  errors  err_rate
08:00                   102       7      6.9%
09:00                   101       3      3.0%
10:00                   106       5      4.7%
11:00                    91       2      2.2%
12:00                    93       2      2.2%
13:00                   107       5      4.7%
```

## Q2 — Latency percentiles (p50 / p95 / p99)

Averages hide tail latency; percentiles don't. Ask for fleet-wide percentiles,
and break out p95 per service with a `terms` + `percentiles` sub-agg to see who
owns the tail.

```json
POST /logs-app/_search
{
  "size": 0,
  "aggs": {
    "latency": { "percentiles": { "field": "latency_ms", "percents": [50, 95, 99] } },
    "by_service": {
      "terms": { "field": "service" },
      "aggs": { "p95": { "percentiles": { "field": "latency_ms", "percents": [95] } } }
    }
  }
}
```

The fleet-wide block comes back as ES-style keyed percentiles:

```json
{ "latency": { "values": { "50.0": 56.0, "95.0": 321.15, "99.0": 565.22 } } }
```

Per-service p95 makes the culprits obvious — `checkout` and `payments` carry
the tail while `catalog` and `search` stay flat:

```
fleet-wide  p50=56  p95=321  p99=565
service        p95 latency
catalog                51
search                 79
checkout              447
payments              529
```

## Q3 — Top services by volume (with latency stats)

One `terms` agg ranks services by event count; a `stats` sub-agg attaches
count/avg/min/max latency to each in the same round trip — leaderboard and
latency profile from a single query.

```json
POST /logs-app/_search
{
  "size": 0,
  "aggs": {
    "top_services": {
      "terms": { "field": "service", "size": 10, "order": { "_count": "desc" } },
      "aggs": { "lat": { "stats": { "field": "latency_ms" } } }
    }
  }
}
```

```
service       volume   avg_ms   min    max
catalog          190       35    15    118
search           187       51    22    114
checkout         150      167    60    590
payments          73      269   103    958
```

`catalog` and `search` take the most traffic but are cheap; `payments` is
low-volume but has the worst worst-case (958 ms). That's the whole triage
picture from three `_search` calls.

## Reproduce it yourself

The tables above are the real, captured output of this exact script. The dataset
is generated with a fixed seed (`random.seed(7)`), so the counts, error rates,
percentiles, and per-service tables are **deterministic** — you will get the same
numbers shown here. The only run-to-run variance is `took` (per-query latency),
which stays in the single-digit milliseconds (this run: Q1 `took=4 ms`, Q2
`took=5 ms`).

```bash
# 1. Start XERJ on its default port (9200), insecure/local — no config needed
./engine/target/release/xerj --insecure --data-dir /tmp/xerj-logs &

# 2. Run the recipe (stdlib only — no pip installs, no third-party network calls)
python3 docs/examples/log-analytics/log_analytics.py
```

The client reads the server URL from `$XERJ_URL` (default
`http://localhost:9200`). A positional argument still overrides it if you run
XERJ on a different port, e.g.
`python3 docs/examples/log-analytics/log_analytics.py http://localhost:9483`.

The script creates the mapping, bulk-ingests 600 events, runs all three queries,
and prints the tables above. It asserts the ingest count and that `_bulk`
reported no errors, so a green run means the aggregations are real.

A green run prints the three per-question tables shown above and ends with
`All aggregations returned.` — the deterministic headline numbers it produces,
summarized:

```
Ingested 600 log events into 'logs-app'

Q1  hourly error rate:  08:00 6.9% · 09:00 3.0% · 10:00 4.7% · 11:00 2.2% · 12:00 2.2% · 13:00 4.7%
Q2  fleet latency:      p50=56 ms  p95=321 ms  p99=565 ms   (per-service p95: catalog 51, search 79, checkout 447, payments 529)
Q3  top services:       catalog 190 · search 187 · checkout 150 · payments 73   (payments worst-case 958 ms)
```

## Notes and honest limits

- These aggregations are computed on demand per query — XERJ is not running a
  continuous rollup or a background job. That's exactly what you want for
  ad-hoc dashboards and alerting queries; if you need pre-materialized rollups
  at very high cardinality, aggregate on a schedule and index the results.
- `fixed_interval: "1h"` gives evenly spaced buckets; use `calendar_interval`
  (`"day"`, `"month"`) when you want calendar-aware boundaries and DST handling.
- Percentiles use an approximate (t-digest-style) algorithm, matching ES
  semantics — the values are estimates, not exact order statistics, which is
  why p95 reads as `321.15` rather than a whole millisecond.
- Same documents, same engine: because `message` is a `text` field you can add
  `match`/`match_phrase` full-text filters to any of these queries (e.g. only
  aggregate logs matching `"timeout"`) without moving data anywhere.
