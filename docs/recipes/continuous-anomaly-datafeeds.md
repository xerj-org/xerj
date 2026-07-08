# Monitor a metric continuously with a datafeed

**Goal:** you already know how to score a metric series for anomalies on demand
(see [Detect a spike in a metric series](./anomaly-detection.md) — you POST
`_score` and get the whole timeline back). But a dashboard or an alerting loop
doesn't want to *ask* every time; it wants the engine to keep watching a live
index and hand you new anomalies as they appear. That's a **datafeed**.

XERJ ships the Elasticsearch `_ml/datafeeds` surface. A datafeed references an
existing anomaly-detection job (a detector), reads from one or more `indices`
through an optional `query`, and runs on a fixed `frequency`. When you `_start`
it, XERJ scores the series once immediately and then keeps re-scoring on a timer,
appending only the **newly** flagged buckets to the job's results — which you
read back with `GET /_ml/anomaly_detectors/{job}/results/records`. Same
transparent moving mean/stddev z-score as the on-demand path; now it runs
itself.

> **Honest scope.** The scoring is the same deterministic moving-window z-score
> as `_score` — a great fit for "this metric jumped relative to its recent
> self," not a seasonal/multivariate model. Results are held **in memory**
> (bounded to the last ~10k records per job) and the background scorer is a
> per-node **tokio task**: results and running datafeeds do **not** survive a
> restart (a reloaded datafeed comes back `stopped`, and `_start` begins a fresh
> run). This is single-node — there is no cross-node datafeed scheduler. It is
> exactly what you want for a live dashboard or a self-contained alert loop on
> one node; it is not a replacement for a clustered, durable ML pipeline.

## The scenario

`cpu_datafeed` collects one CPU reading per minute. For sixteen minutes it sits
around 20%, with a runaway spike to **96%** at minute 12. We create a detector,
attach a datafeed, and start it. Then — while the datafeed is running — a
**second** spike (88%) lands at minute 20, and we watch the datafeed find it on
its own.

## 1. Map the index and create the detector

The time field must be a `date`; the metric must be numeric. The detector is the
same object as in the on-demand recipe: `mean(cpu)` over one-minute buckets.

```bash
curl -XPUT localhost:9486/cpu_datafeed -H 'Content-Type: application/json' -d '{
  "mappings": { "properties": {
    "@timestamp": { "type": "date" },
    "cpu":        { "type": "double" }
  }}
}'

curl -XPUT localhost:9486/_ml/anomaly_detectors/cpu-spike \
  -H 'Content-Type: application/json' -d '{
    "source_index": "cpu_datafeed",
    "time_field":   "@timestamp",
    "function":     "mean",
    "field":        "cpu",
    "bucket_span":  "1m"
  }'
```

## 2. Create the datafeed

A datafeed needs a `job_id` (which must reference an existing detector — else
`404`). `indices` defaults to the detector's `source_index`, `query` to
`match_all`, and `frequency` parses an ES time value (`"2s"`, `"5m"`, …; clamped
to `>= 1s`, default `60s`).

```bash
curl -XPUT localhost:9486/_ml/datafeeds/cpu-spike-feed \
  -H 'Content-Type: application/json' -d '{
    "job_id":    "cpu-spike",
    "frequency": "2s"
  }'
```

Response (the datafeed config, ES-shaped):

```json
{
  "datafeed_id": "cpu-spike-feed",
  "job_id": "cpu-spike",
  "state": "stopped",
  "indices": ["cpu_datafeed"],
  "query": { "match_all": {} },
  "frequency": "2s"
}
```

## 3. Start it — one pass now, then a timer

`_start` marks the datafeed `started`, runs **one scoring pass synchronously**
(so results are immediately queryable), and spawns a background task that
re-scores every `frequency`.

```bash
curl -XPOST localhost:9486/_ml/datafeeds/cpu-spike-feed/_start
# -> {"started": true}
```

Read the results the first pass produced:

```bash
curl 'localhost:9486/_ml/anomaly_detectors/cpu-spike/results/records'
```

```json
{
  "count": 1,
  "records": [
    {
      "job_id": "cpu-spike",
      "timestamp": 1783296720000,
      "timestamp_iso": "2026-07-06T00:12:00.000Z",
      "actual": 96.0,
      "typical": 20.25,
      "expected": 20.25,
      "std_dev": 1.1636866703140785,
      "z_score": 65.09484205018445,
      "record_score": 100.0,
      "bucket_span": "1m",
      "is_anomaly": true
    }
  ]
}
```

Each record reuses the fields the `_score` endpoint emits per bucket, plus the
ES `record_score` (the 0–100 severity) and `typical` (ES's name for the
baseline `expected`).

## 4. The continuous part

Now push a **new** spike into the live index — without touching the datafeed:

```bash
curl -XPOST 'localhost:9486/_bulk?refresh=true' \
  -H 'Content-Type: application/x-ndjson' --data-binary $'
{"index":{"_index":"cpu_datafeed"}}
{"@timestamp":"2026-07-06T00:20:00Z","cpu":88}
'
```

Within one `frequency` tick the background scorer re-buckets the series, sees the
new bucket, and appends it. Poll again a couple of seconds later:

```bash
curl 'localhost:9486/_ml/anomaly_detectors/cpu-spike/results/records'
# -> count: 2  (the 00:12 spike AND the new 00:20 spike)
```

The datafeed tracks the highest bucket timestamp it has already scored, so each
pass appends only the **new** anomalies — the 00:12 record is never duplicated.

> **A note on freshness.** XERJ's read path caches aggregation results keyed by
> `(query, dataset_version)`, and version only bumps on a segment flush — so an
> *identical* query repeated against an index that has only taken unflushed
> (in-memory) writes can be served a cached result. Because a datafeed re-issues
> the same query every tick, its scorer deliberately requests an **uncached**
> read so it always sees the newest buckets. (The on-demand `_score` endpoint is
> unchanged and remains cache-eligible.)

## 5. Filter and cap the results

`results/records` accepts a `record_score` floor (query param or JSON body) and a
`size` cap:

```bash
# only high-severity records
curl 'localhost:9486/_ml/anomaly_detectors/cpu-spike/results/records?record_score=90'
# at most 5, most-recent-friendly (chronological)
curl 'localhost:9486/_ml/anomaly_detectors/cpu-spike/results/records?size=5'
```

## 6. Stop and delete

```bash
curl -XPOST localhost:9486/_ml/datafeeds/cpu-spike-feed/_stop
# -> {"stopped": true}   (aborts the background task)

curl localhost:9486/_cat/ml/datafeeds
# cpu-spike-feed stopped cpu-spike

curl -XDELETE localhost:9486/_ml/datafeeds/cpu-spike-feed
# {"acknowledged": true}
```

A datafeed must be stopped before it can be deleted — deleting a `started`
datafeed returns `409`, exactly like Elasticsearch.

## Run it

```bash
# XERJ listening on :9486 (adjust XERJ=... for another port)
python3 docs/examples/continuous-anomaly-datafeeds/datafeed_demo.py
```

The script maps the index, ingests the series with the first spike, creates the
detector and datafeed, starts it, reads the first anomaly, injects the second
spike into the live index, polls until the running datafeed picks it up on its
own, then stops and **asserts** both spikes were detected — the second without a
second `_start`. It exits non-zero if any of that regresses, so it doubles as a
smoke test.

Expected output:

```
datafeed cpu-spike-feed -> job cpu-spike (every 2s), state=stopped

after _start — 1 anomaly record(s) from the first pass:
  when                   actual   expected   record_score
  2026-07-06 00:12 UTC     96.0       20.2          100.0

injecting a new spike at 00:20, waiting for the 2s scorer...
datafeed picked up 1 new anomaly record(s) on its own:
  when                   actual   expected   record_score
  2026-07-06 00:20 UTC     88.0       20.2          100.0

_stop -> {'stopped': True} · datafeed state now: stopped

OK: 2 anomalies detected continuously; datafeed stopped cleanly.
```

## Endpoint cheat-sheet

| Method | Path | Purpose |
|---|---|---|
| `PUT` | `/_ml/datafeeds/{id}` | create/replace a datafeed (`job_id` required; `indices`, `query`, `frequency` optional) |
| `GET` | `/_ml/datafeeds/{id}` | fetch one datafeed |
| `GET` | `/_ml/datafeeds` | list all datafeeds (`{datafeeds, count}`) |
| `DELETE` | `/_ml/datafeeds/{id}` | remove a datafeed (must be stopped, else `409`) |
| `POST` | `/_ml/datafeeds/{id}/_start` | start: score now + re-score every `frequency` |
| `POST` | `/_ml/datafeeds/{id}/_stop` | stop and abort the background scorer |
| `GET` | `/_ml/anomaly_detectors/{job}/results/records` | stored anomaly records; `record_score` / `size` filters |
| `GET` | `/_cat/ml/datafeeds` | one line per datafeed: `id state job_id` |

See [Detect a spike in a metric series](./anomaly-detection.md) for the detector
model and the scoring math these records come from.
