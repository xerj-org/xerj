#!/usr/bin/env python3
"""
Log analytics with XERJ — from raw logs to dashboards.

Bulk-ingests a few hundred synthetic structured log events, then answers
real operational questions with Elasticsearch-style aggregations:
  1. Error rate over time      -> date_histogram + filters sub-agg
  2. p95 / p99 latency         -> percentiles
  3. Top services by volume    -> terms + stats sub-agg

Stdlib only (urllib + json). No pip installs. Point it at a live XERJ.
"""
import json
import os
import random
import sys
import urllib.request
from datetime import datetime, timedelta, timezone

# Server URL resolution (no un-overridable hardcoded port):
#   1. explicit argv[1]                      (documented positional form)
#   2. $XERJ_URL environment variable        (uniform across all recipes)
#   3. http://localhost:9200                 (XERJ's default es_compat_port)
BASE = (sys.argv[1] if len(sys.argv) > 1
        else os.environ.get("XERJ_URL", "http://localhost:9200"))
INDEX = "logs-app"


def req(method, path, body=None, ndjson=False):
    data = None
    headers = {}
    if ndjson:
        data = body.encode()
        headers["Content-Type"] = "application/x-ndjson"
    elif body is not None:
        data = json.dumps(body).encode()
        headers["Content-Type"] = "application/json"
    r = urllib.request.Request(BASE + path, data=data, headers=headers, method=method)
    with urllib.request.urlopen(r) as resp:
        return json.loads(resp.read().decode())


# ---------------------------------------------------------------- setup
# Clean slate, then create an explicit mapping so timestamp is a date,
# latency_ms is numeric, and service/level/status are keywords for aggs.
try:
    req("DELETE", "/" + INDEX)
except urllib.error.HTTPError:
    pass

req("PUT", "/" + INDEX, {
    "mappings": {
        "properties": {
            "@timestamp": {"type": "date"},
            "level":      {"type": "keyword"},
            "service":    {"type": "keyword"},
            "latency_ms": {"type": "integer"},
            "status":     {"type": "integer"},
            "message":    {"type": "text"},
        }
    }
})

# ---------------------------------------------------------------- generate
# 600 events across 6 hours and 4 services. Each service has its own
# baseline latency and error propensity so the aggregations tell a story.
random.seed(7)
services = {
    "checkout":  {"lat": 120, "err": 0.08},
    "search":    {"lat": 45,  "err": 0.02},
    "catalog":   {"lat": 30,  "err": 0.01},
    "payments":  {"lat": 200, "err": 0.05},
}
start = datetime(2026, 7, 6, 8, 0, 0, tzinfo=timezone.utc)
N = 600
lines = []
for i in range(N):
    svc = random.choices(list(services), weights=[3, 4, 4, 2])[0]
    cfg = services[svc]
    ts = start + timedelta(seconds=random.randint(0, 6 * 3600))
    is_err = random.random() < cfg["err"]
    if is_err:
        level, status = "ERROR", random.choice([500, 502, 503])
        latency = int(cfg["lat"] * random.uniform(2.0, 5.0))  # errors are slow
    else:
        level = random.choices(["INFO", "WARN"], weights=[9, 1])[0]
        status = 200
        latency = int(cfg["lat"] * random.uniform(0.5, 1.8))
    doc = {
        "@timestamp": ts.isoformat(),
        "level": level, "service": svc,
        "latency_ms": latency, "status": status,
        "message": f"{svc} handled request status={status}",
    }
    lines.append(json.dumps({"index": {"_index": INDEX}}))
    lines.append(json.dumps(doc))

resp = req("POST", "/_bulk", "\n".join(lines) + "\n", ndjson=True)
assert not resp.get("errors"), "bulk had errors: " + json.dumps(resp)[:400]
req("POST", "/" + INDEX + "/_refresh")
count = req("GET", "/" + INDEX + "/_count")["count"]
print(f"Ingested {count} log events into '{INDEX}'\n")
assert count == N, f"expected {N}, got {count}"

# ---------------------------------------------------------------- Q1
# Error rate over time: bucket by hour, and inside each bucket count how
# many were ERROR with a `filter` sub-agg. errors/doc_count = error rate.
print("=" * 60)
print("Q1. Error rate over time (hourly)")
print("=" * 60)
r = req("POST", "/" + INDEX + "/_search", {
    "size": 0,
    "aggs": {
        "per_hour": {
            "date_histogram": {"field": "@timestamp", "fixed_interval": "1h"},
            "aggs": {
                "errors": {"filter": {"term": {"level": "ERROR"}}}
            }
        }
    }
})
print(f"(took={r.get('took')} ms)")
print(f"{'hour (UTC)':<22}{'total':>8}{'errors':>8}{'err_rate':>10}")
for b in r["aggregations"]["per_hour"]["buckets"]:
    total = b["doc_count"]
    errs = b["errors"]["doc_count"]
    rate = (errs / total * 100) if total else 0.0
    hour = b["key_as_string"][11:16]
    print(f"{hour:<22}{total:>8}{errs:>8}{rate:>9.1f}%")

# ---------------------------------------------------------------- Q2
# Latency percentiles across the whole fleet, then p95 broken out per
# service so you can see which service is dragging the tail.
print()
print("=" * 60)
print("Q2. Latency percentiles (ms)")
print("=" * 60)
r = req("POST", "/" + INDEX + "/_search", {
    "size": 0,
    "aggs": {
        "latency": {"percentiles": {"field": "latency_ms", "percents": [50, 95, 99]}},
        "by_service": {
            "terms": {"field": "service"},
            "aggs": {"p95": {"percentiles": {"field": "latency_ms", "percents": [95]}}}
        }
    }
})
pct = r["aggregations"]["latency"]["values"]
raw = "  ".join(f"{k}={v:.2f}" for k, v in sorted(pct.items(), key=lambda kv: float(kv[0])))
print(f"(took={r.get('took')} ms; raw percentiles: {raw})")
print(f"fleet-wide  p50={pct['50.0']:.0f}  p95={pct['95.0']:.0f}  p99={pct['99.0']:.0f}")
print(f"{'service':<12}{'p95 latency':>14}")
for b in r["aggregations"]["by_service"]["buckets"]:
    p95 = b["p95"]["values"]["95.0"]
    print(f"{b['key']:<12}{p95:>13.0f}")

# ---------------------------------------------------------------- Q3
# Top services by volume, with a stats sub-agg (count/avg/min/max) so one
# call gives you the leaderboard AND the latency profile of each service.
print()
print("=" * 60)
print("Q3. Top services by volume (+ latency stats)")
print("=" * 60)
r = req("POST", "/" + INDEX + "/_search", {
    "size": 0,
    "aggs": {
        "top_services": {
            "terms": {"field": "service", "size": 10, "order": {"_count": "desc"}},
            "aggs": {"lat": {"stats": {"field": "latency_ms"}}}
        }
    }
})
print(f"{'service':<12}{'volume':>8}{'avg_ms':>9}{'min':>6}{'max':>7}")
for b in r["aggregations"]["top_services"]["buckets"]:
    s = b["lat"]
    print(f"{b['key']:<12}{b['doc_count']:>8}{s['avg']:>9.0f}{int(s['min']):>6}{int(s['max']):>7}")

print("\nAll aggregations returned. Log analytics on one engine, ES wire API.")
