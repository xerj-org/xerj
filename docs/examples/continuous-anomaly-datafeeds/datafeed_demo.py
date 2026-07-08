#!/usr/bin/env python3
"""
Continuous anomaly detection with an XERJ _ml datafeed.

The on-demand `_score` API scores a series once, on request. A *datafeed* turns
that into a live monitor: it references a detector, and once started it re-scores
the source index every `frequency`, appending only the NEW flagged buckets to the
job's results store — which you poll with
`GET /_ml/anomaly_detectors/{job}/results/records`. Same transparent moving
mean/stddev z-score, now on a timer.

This demo proves the "continuous" part end to end:
  1. ingest a metric series with one spike, create a detector + datafeed,
  2. _start it (one scoring pass runs immediately) and read the first anomaly,
  3. ingest a SECOND spike into the live index,
  4. poll until the background scorer picks it up — with no second _start.

Run:  python3 datafeed_demo.py                 # assumes XERJ on :9486
      XERJ=http://localhost:9200 python3 datafeed_demo.py

Uses only the Python 3 standard library. No pip installs.
"""

import datetime
import os
import sys
import time
import json
import urllib.error
import urllib.request

XERJ = os.environ.get("XERJ", "http://localhost:9486").rstrip("/")
INDEX = "cpu_datafeed"
DETECTOR = "cpu-spike"
DATAFEED = "cpu-spike-feed"
FREQUENCY = "2s"  # re-score cadence for the background scorer


def call(method, path, body=None):
    data = None
    headers = {}
    if body is not None:
        data = body.encode() if isinstance(body, str) else json.dumps(body).encode()
        headers["Content-Type"] = (
            "application/x-ndjson" if isinstance(body, str) else "application/json"
        )
    req = urllib.request.Request(f"{XERJ}{path}", data=data, headers=headers, method=method)
    with urllib.request.urlopen(req) as resp:
        return json.loads(resp.read())


def ingest(readings):
    """Ingest [(minute, cpu)] as one doc per minute (one reading per bucket)."""
    lines = []
    for minute, cpu in readings:
        ts = "2026-07-06T00:%02d:00Z" % minute
        lines.append(json.dumps({"index": {"_index": INDEX}}))
        lines.append(json.dumps({"@timestamp": ts, "cpu": cpu}))
    call("POST", "/_bulk?refresh=true", "\n".join(lines) + "\n")


def when(record):
    ts = datetime.datetime.fromtimestamp(record["timestamp"] / 1000, datetime.timezone.utc)
    return ts.strftime("%Y-%m-%d %H:%M UTC")


def show(records):
    print("  when                   actual   expected   record_score")
    for r in sorted(records, key=lambda r: r["timestamp"]):
        print("  %-20s %8.1f %10.1f %14.1f"
              % (when(r), r["actual"], r["expected"], r["record_score"]))


def poll_records(min_count, timeout_s=15):
    """Poll results/records until at least `min_count` are stored (or time out)."""
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        recs = call("GET", f"/_ml/anomaly_detectors/{DETECTOR}/results/records")["records"]
        if len(recs) >= min_count:
            return recs
        time.sleep(1)
    return call("GET", f"/_ml/anomaly_detectors/{DETECTOR}/results/records")["records"]


def main():
    # Fresh index: @timestamp (bucketing clock) + cpu (the metric we average).
    try:
        call("DELETE", f"/{INDEX}")
    except urllib.error.HTTPError:
        pass
    call("PUT", f"/{INDEX}", {
        "mappings": {"properties": {
            "@timestamp": {"type": "date"},
            "cpu": {"type": "double"},
        }}
    })

    # Baseline ~20% CPU for 16 minutes, with a runaway spike to 96 at minute 12.
    baseline = [19, 21, 20, 20, 22, 18, 21, 20, 19, 21, 20, 22, 96, 20, 21, 19]
    ingest(list(enumerate(baseline)))

    # A detector is which index, which clock field, what metric, bucket size,
    # and how many stddevs count as anomalous.
    call("PUT", f"/_ml/anomaly_detectors/{DETECTOR}", {
        "source_index": INDEX,
        "time_field": "@timestamp",
        "function": "mean",
        "field": "cpu",
        "bucket_span": "1m",
        "anomaly_threshold": 3.0,
        "description": "per-minute CPU mean vs moving baseline",
    })

    # A datafeed points a detector at a live index and runs it on a timer.
    feed = call("PUT", f"/_ml/datafeeds/{DATAFEED}", {
        "job_id": DETECTOR,
        "frequency": FREQUENCY,
    })
    print(f"datafeed {feed['datafeed_id']} -> job {feed['job_id']} "
          f"(every {feed['frequency']}), state={feed['state']}\n")

    # Start it: one scoring pass runs synchronously now, then a background task
    # re-scores every `frequency`.
    call("POST", f"/_ml/datafeeds/{DATAFEED}/_start")
    first = poll_records(1)
    print(f"after _start — {len(first)} anomaly record(s) from the first pass:")
    show(first)

    # Now the "continuous" part: a NEW spike lands in the live index at minute
    # 20. We do NOT touch the datafeed — the running background scorer should
    # find it on its next tick.
    print(f"\ninjecting a new spike at 00:20, waiting for the {FREQUENCY} scorer...")
    ingest([(16, 20), (17, 21), (18, 19), (19, 20), (20, 88), (21, 20)])
    after = poll_records(len(first) + 1)
    new = [r for r in after if r not in first]
    print(f"datafeed picked up {len(new)} new anomaly record(s) on its own:")
    show(new)

    # Stop the datafeed (idempotent cleanup for re-runs).
    stopped = call("POST", f"/_ml/datafeeds/{DATAFEED}/_stop")
    fed = call("GET", f"/_ml/datafeeds/{DATAFEED}")["datafeeds"][0]
    print(f"\n_stop -> {stopped} · datafeed state now: {fed['state']}")

    # Verify: both spikes were detected, the second WITHOUT re-starting.
    spikes = {when(r): r["actual"] for r in after}
    assert any(96.0 == v for v in spikes.values()), "first spike (00:12) missing"
    assert any(88.0 == v for v in spikes.values()), "continuously-detected spike (00:20) missing"
    assert len(after) >= 2, f"expected >=2 anomaly records, got {len(after)}"
    assert fed["state"] == "stopped", "datafeed did not stop"
    print(f"\nOK: {len(after)} anomalies detected continuously; datafeed stopped cleanly.")


if __name__ == "__main__":
    try:
        main()
    except AssertionError as e:
        print(f"FAILED: {e}", file=sys.stderr)
        sys.exit(1)
