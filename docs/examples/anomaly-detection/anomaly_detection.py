#!/usr/bin/env python3
"""
Anomaly detection on a metric series with XERJ.

Scenario: we collect per-host CPU readings into `cpu_metrics`. For fifteen
minutes CPU sits around ~20%. At minute 12 a runaway process pins the box and
the per-minute mean jumps to ~96%, then recovers. We want XERJ to flag that one
minute as anomalous - automatically, without us hand-picking a threshold on the
raw value.

XERJ's on-demand anomaly detector does exactly this: it buckets the source
index by time, computes the metric per bucket (here mean(cpu)), builds a moving
baseline from prior *normal* buckets, and scores each bucket by how many
standard deviations it sits from that baseline.

Run:  python3 anomaly_detection.py            # assumes XERJ on :9486
      XERJ=http://localhost:9200 python3 anomaly_detection.py

Uses only the Python 3 standard library. No pip installs.
"""
import json
import os
import urllib.request

XERJ = os.environ.get("XERJ", "http://localhost:9486").rstrip("/")
INDEX = "cpu_metrics"
JOB = "cpu-spike"


def req(method, path, body=None, ndjson=False):
    data = None
    headers = {}
    if ndjson:
        data = body.encode()
        headers["Content-Type"] = "application/x-ndjson"
    elif body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    r = urllib.request.Request(XERJ + path, data=data, method=method, headers=headers)
    with urllib.request.urlopen(r) as resp:
        return json.loads(resp.read().decode())


# 1. Map the index: @timestamp as a date, cpu as a numeric we can average.
try:
    req("DELETE", "/" + INDEX)
except Exception:
    pass
req("PUT", "/" + INDEX, {
    "mappings": {
        "properties": {
            "@timestamp": {"type": "date"},
            "host":       {"type": "keyword"},
            "cpu":        {"type": "double"},
        }
    }
})

# 2. Build 16 one-minute buckets of 4 host readings each.
#    Minutes 0-11 and 13-15 are normal (~20%); minute 12 is the injected spike.
#    Readings per minute are chosen so the per-minute mean is exact & stable.
NORMAL = [19, 20, 20, 21]          # mean 20.0
JITTER = {                          # a little bucket-to-bucket variance
    0: 0, 1: 1, 2: -1, 3: 0, 4: 2, 5: -2, 6: 1, 7: 0,
    8: -1, 9: 1, 10: 0, 11: 2, 13: 0, 14: 1, 15: -1,
}
SPIKE = [94, 96, 96, 98]           # mean 96.0  <-- the anomaly

lines = []
for minute in range(16):
    ts = "2026-07-06T00:%02d:00Z" % minute
    if minute == 12:
        readings = SPIKE
    else:
        readings = [v + JITTER[minute] for v in NORMAL]
    for h, cpu in enumerate(readings):
        lines.append(json.dumps({"index": {"_index": INDEX}}))
        lines.append(json.dumps({"@timestamp": ts, "host": "web-%d" % h, "cpu": cpu}))
bulk = req("POST", "/_bulk", "\n".join(lines) + "\n", ndjson=True)
assert not bulk.get("errors"), bulk
req("POST", "/" + INDEX + "/_refresh")

# 3. Create the detector: mean(cpu) over 1-minute buckets.
job = req("PUT", "/_ml/anomaly_detectors/" + JOB, {
    "source_index": INDEX,
    "time_field": "@timestamp",
    "function": "mean",
    "field": "cpu",
    "bucket_span": "1m",
    "description": "CPU mean per minute",
})
print("== detector created ==")
print(json.dumps({
    "job_id": job["job_id"],
    "function": job["analysis_config"]["detectors"][0]["function"],
    "bucket_span": job["analysis_config"]["bucket_span"],
    "anomaly_threshold": job["anomaly_threshold"],
}, indent=2))

# 4. Score it. Baseline warm-up is the first 4 buckets (no score yet); every
#    later bucket is scored against the mean/std of prior *normal* buckets.
res = req("POST", "/_ml/anomaly_detectors/" + JOB + "/_score")

print("\n== per-bucket scores ==")
print("%-20s %8s %9s %8s %7s %5s" %
      ("minute (UTC)", "actual", "expected", "z_score", "score", "flag"))
for r in res["records"]:
    exp = r["expected"]
    z = r["z_score"]
    print("%-20s %8.1f %9s %8s %7.1f %5s" % (
        r["timestamp_iso"][11:19],
        r["actual"],
        "-" if exp is None else "%.1f" % exp,
        "-" if z is None else "%.1f" % z,
        r["anomaly_score"],
        "***" if r["is_anomaly"] else "",
    ))

print("\n== anomalies (%d of %d buckets) ==" % (res["anomaly_count"], res["bucket_count"]))
for a in res["anomalies"]:
    print("  %s  actual=%.1f expected=%.1f  z=%.1f  score=%.1f" % (
        a["timestamp_iso"], a["actual"], a["expected"], a["z_score"], a["anomaly_score"]))

# 5. Verify the result is correct.
spike = next(r for r in res["records"] if r["timestamp_iso"].startswith("2026-07-06T00:12"))
normals = [r for r in res["records"]
           if r["expected"] is not None and not r["timestamp_iso"].startswith("2026-07-06T00:12")]

assert res["anomaly_count"] == 1, "expected exactly one anomaly, got %d" % res["anomaly_count"]
assert spike["is_anomaly"], "spike bucket was not flagged"
assert spike["anomaly_score"] >= 99.0, "spike score too low: %s" % spike["anomaly_score"]
assert abs(spike["actual"] - 96.0) < 1e-6, "spike mean wrong: %s" % spike["actual"]
# is_anomaly is the gate: only the spike crosses the threshold; jitter buckets
# never do, even if their normalized score creeps up near the threshold line.
assert all(not r["is_anomaly"] for r in normals), "a normal bucket was flagged"
max_normal = max(r["anomaly_score"] for r in normals)
assert spike["anomaly_score"] >= 2 * max_normal, \
    "spike (%.1f) not clearly separated from top normal (%.1f)" % (
        spike["anomaly_score"], max_normal)
print("\nOK: spike at 00:12 flagged (score %.1f); %d normal buckets, none flagged"
      " (top normal score %.1f)." % (spike["anomaly_score"], len(normals), max_normal))
