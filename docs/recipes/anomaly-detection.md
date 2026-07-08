# Detect a spike in a metric series

**Goal:** you have a timestamped metric — CPU, latency, request rate, error
count — and you want to know *when it went wrong* without hand-tuning a fixed
red-line. A CPU box that idles at 20% is fine at 20% and alarming at 96%, but
the "alarming" number depends on the box. You want the engine to learn the
normal band and flag the minute it broke out.

XERJ ships an on-demand anomaly detector (`_ml/anomaly_detectors`, Elasticsearch
wire-shape). You point it at an index, tell it the time field, the metric
(`mean(cpu)`), and a bucket span. When you score it, XERJ buckets the data by
time, computes the metric per bucket, builds a **moving baseline from prior
normal buckets**, and scores each bucket by how many standard deviations it sits
from that baseline. A spike lands many sigmas out and gets flagged; ordinary
jitter does not.

> **Honest scope.** This is **on-demand batch scoring**: you POST `_score` and
> get the full timeline back in one response. There is **no continuous datafeed
> or background job** yet — nothing runs on a timer, and there is no alerting.
> To monitor live, call `_score` on a schedule (cron, your alertmanager, a
> `/loop`) over a recent time window and act on `anomaly_count`. The model is a
> transparent moving-window z-score, not a seasonal/multivariate ML model — it's
> excellent for "this metric jumped relative to its recent self," which covers
> the large majority of ops spikes.

## The scenario

`cpu_metrics` collects four host readings per minute. For fifteen minutes CPU
sits around 20%. At **minute 12** a runaway process pins a box and the per-minute
mean jumps to **96%**, then recovers. We want XERJ to flag exactly that minute.

## 1. Map the index

The time field must be a `date`; the metric must be numeric so the detector can
average it.

```bash
curl -XPUT localhost:9200/cpu_metrics -H 'Content-Type: application/json' -d '{
  "mappings": { "properties": {
    "@timestamp": { "type": "date" },
    "host":       { "type": "keyword" },
    "cpu":        { "type": "double" }
  }}
}'
```

## 2. Bulk-ingest the series (spike injected at minute 12)

Standard `_bulk` NDJSON — 16 minutes × 4 hosts. Normal minutes read ~20%; minute
12 reads ~96%. (Full generator in the script below.)

```
{"index":{"_index":"cpu_metrics"}}
{"@timestamp":"2026-07-06T00:00:00Z","host":"web-0","cpu":19}
...
{"index":{"_index":"cpu_metrics"}}
{"@timestamp":"2026-07-06T00:12:00Z","host":"web-1","cpu":96}   <- the spike
...
```

## 3. Create the detector

`function: mean` over the `cpu` field, one-minute buckets. `anomaly_threshold`
is the z-score gate (defaults to `3.0` — a bucket is flagged when it sits more
than 3σ from the baseline).

```bash
curl -XPUT localhost:9200/_ml/anomaly_detectors/cpu-spike \
  -H 'Content-Type: application/json' -d '{
    "source_index": "cpu_metrics",
    "time_field":   "@timestamp",
    "function":     "mean",
    "field":        "cpu",
    "bucket_span":  "1m",
    "description":  "CPU mean per minute"
  }'
```

Response (the config, ES-shaped):

```json
{
  "job_id": "cpu-spike",
  "job_type": "anomaly_detector",
  "analysis_config": {
    "bucket_span": "1m",
    "detectors": [{ "function": "mean", "field_name": "cpu" }]
  },
  "data_description": { "time_field": "@timestamp" },
  "source_index": ["cpu_metrics"],
  "anomaly_threshold": 3.0
}
```

Supported `function` values: `count` (no field needed — buckets by doc count,
great for request/error rate), `mean`/`avg`, `min`, `max`, `sum`.

## 4. Score it

```bash
curl -XPOST localhost:9200/_ml/anomaly_detectors/cpu-spike/_score
```

XERJ returns the whole timeline plus a ranked `anomalies` list. Rendered per
bucket:

```
minute (UTC)           actual  expected  z_score   score  flag
00:00:00                 20.0         -        -     0.0        <- warm-up
00:01:00                 21.0         -        -     0.0        <- warm-up
00:02:00                 19.0         -        -     0.0        <- warm-up
00:03:00                 20.0         -        -     0.0        <- warm-up
00:04:00                 22.0      20.0      2.8    48.0
00:05:00                 18.0      20.4     -2.4    41.9
00:06:00                 21.0      20.0      0.8    16.4
00:07:00                 20.0      20.1     -0.1     2.6
00:08:00                 19.0      20.1     -1.0    20.0
00:09:00                 21.0      20.0      0.9    18.1
00:10:00                 20.0      20.1     -0.1     2.0
00:11:00                 22.0      20.1      1.8    33.5
00:12:00                 96.0      20.2     65.1   100.0   ***  <- SPIKE
00:13:00                 20.0      20.2     -0.2     4.8
00:14:00                 21.0      20.2      0.7    14.7
00:15:00                 19.0      20.3     -1.2    23.7

== anomalies (1 of 16 buckets) ==
  2026-07-06T00:12:00.000Z  actual=96.0 expected=20.2  z=65.1  score=100.0
```

The spike record in full:

```json
{
  "actual": 96.0,
  "expected": 20.25,
  "std_dev": 1.1636866703140785,
  "z_score": 65.09484205018445,
  "anomaly_score": 100.0,
  "is_anomaly": true,
  "timestamp_iso": "2026-07-06T00:12:00.000Z"
}
```

## How the scoring works (and how to read the numbers)

- **Warm-up.** The first 4 buckets seed the baseline and are not scored
  (`"expected": null`, `"note": "insufficient_baseline"`). Give the detector a
  span of history before the interesting part.
- **`expected` / `std_dev`.** The mean and standard deviation of prior **normal**
  buckets. Crucially, a flagged bucket is **not folded back into the baseline** —
  one spike can't poison the expectation for the minutes after it. Notice
  `expected` stays ~20 straight through the spike.
- **`z_score`.** `(actual − expected) / std_dev`. The spike is 65σ out; normal
  jitter stays within a couple of sigma.
- **`is_anomaly`** is the decision, gated purely on `|z| > anomaly_threshold`.
  Only the spike crosses it here.
- **`anomaly_score` (0–100)** is a normalized, human-friendly severity: it maps
  |z| = threshold to ~50 and saturates toward 100 for large deviations. It is
  **not** the flag. That's why the `00:04` jitter bucket shows a mid score of 48
  (its z of 2.8 is close to the threshold of 3) yet is correctly *not* flagged.
  **Alert on `is_anomaly` / `anomaly_count`; use `anomaly_score` for ranking and
  dashboards.** In this run the spike (100.0) is more than 2× the top normal
  bucket (48.0) — clean separation.

## Narrow or re-tune a run without recreating the job

`_score` accepts two per-run overrides in the POST body:

- **`query`** — restrict which docs feed the buckets (e.g. one `host`, one
  service). Any XERJ query works: `{"query": {"term": {"host": "web-1"}}}`.
- **`anomaly_threshold`** — override the flag sensitivity for this run only.

```bash
# Same data, stricter gate (10σ) — the 65σ spike still stands alone.
curl -XPOST localhost:9200/_ml/anomaly_detectors/cpu-spike/_score \
  -H 'Content-Type: application/json' -d '{"anomaly_threshold": 10}'
# -> "anomaly_threshold": 10.0, "anomaly_count": 1
```

## Count instead of mean (rate spikes)

For "too many errors this minute" you don't need a metric field — use
`function: count`, drop `field`, and point the detector at your log index. Each
bucket's `actual` becomes the doc count, scored the same way. Same recipe, same
response shape.

## List detectors

```bash
curl localhost:9200/_cat/ml/anomaly_detectors
# cpu-spike opened cpu_metrics mean 1m
```

`GET /_ml/anomaly_detectors/{id}` and `DELETE /_ml/anomaly_detectors/{id}` round
out lifecycle management.

## Reproduce it yourself

Start XERJ on its default port (`9200`) and run the example — no keys, no
external services, stdlib Python only:

```bash
# 1. Start a throwaway XERJ (ES-compat wire on :9200 by default)
xerj --insecure --data-dir ./data

# 2. In another shell, run the demo (honors $XERJ_URL, default http://localhost:9200)
python3 docs/examples/anomaly-detection/anomaly_detection.py
```

Point it at a non-default host/port with `XERJ_URL=http://host:port` (the legacy
`XERJ=...` variable still works as an alias). The data is generated in-script
with a fixed spike, so every run prints the same numbers:

```
== detector created ==
{ "job_id": "cpu-spike", "function": "mean", "bucket_span": "1m", "anomaly_threshold": 3.0 }

00:12:00                 96.0      20.2     65.1   100.0   ***   <- the spike

== anomalies (1 of 16 buckets) ==
  2026-07-06T00:12:00.000Z  actual=96.0 expected=20.2  z=65.1  score=100.0

OK: spike at 00:12 flagged (score 100.0); 11 normal buckets, none flagged (top normal score 48.0).

== spike record (full precision) ==
{ "actual": 96.0, "expected": 20.25, "std_dev": 1.1636866703140785,
  "z_score": 65.09484205018445, "anomaly_score": 100.0, "is_anomaly": true }

== stricter re-score (anomaly_threshold=10) ==
  anomaly_threshold=10.0  anomaly_count=1

== _cat/ml/anomaly_detectors ==
  cpu-spike opened cpu_metrics mean 1m
```

The script maps the index, ingests the series with the injected spike, creates
the detector, scores it, prints the table above, and **asserts** the result:
exactly one anomaly, at 00:12, score 100.0, mean 96.0, 65σ out — and no normal
bucket flagged (the spike's score is 100.0 vs. a top normal of 48.0, clean
separation). It exits non-zero if any of that regresses, so it doubles as a
smoke test.

## Endpoint cheat-sheet

| Method | Path | Purpose |
|---|---|---|
| `PUT` | `/_ml/anomaly_detectors/{id}` | create/replace a detector |
| `GET` | `/_ml/anomaly_detectors/{id}` | fetch one detector's config |
| `GET` | `/_ml/anomaly_detectors` | list all detectors |
| `DELETE` | `/_ml/anomaly_detectors/{id}` | remove a detector |
| `POST` | `/_ml/anomaly_detectors/{id}/_score` | score now; body may set `query`, `anomaly_threshold` |
| `GET` | `/_cat/ml/anomaly_detectors` | one line per detector |

Config fields: `source_index` (required), `time_field` (default `@timestamp`),
`function` (`count`/`mean`/`min`/`max`/`sum`, default `count`), `field`
(required for everything but `count`), `bucket_span` (default `1h`),
`anomaly_threshold` (z-gate, default `3.0`), `description`. ES-style
`analysis_config.detectors[]` and `data_description.time_field` are also
accepted for drop-in configs.
