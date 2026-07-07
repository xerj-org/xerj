#!/usr/bin/env python3
"""Find real brute-force attacks with statistical anomaly detection.

Ingests the public OpenSSH log capture from logpai/loghub that ships in
this repo (655k real events from a production server) and uses XERJ's
`_ml` anomaly-detection API to find the hours where failed-authentication
volume spikes far above its moving baseline — i.e. the actual brute-force
attacks in the capture. No training, no external services: the detector
is a deterministic moving mean/stddev baseline scored per time bucket.

Run it:
    xerj --insecure --data-dir ./data &        # start XERJ
    python3 recipes/log_anomaly.py

Optional env:
    XERJ_URL   (default http://localhost:9200)
"""

import datetime
import json
import os
import pathlib
import urllib.error
import urllib.request

XERJ = os.environ.get("XERJ_URL", "http://localhost:9200")
LOGS = pathlib.Path(__file__).resolve().parent.parent / "engine" / "demo-data" / "ssh_one.ndjson"
INDEX = "ssh-auth"
DETECTOR = "ssh-bruteforce"
BATCH = 5000


def call(method: str, path: str, body=None):
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


def ingest():
    try:
        call("DELETE", f"/{INDEX}")
    except urllib.error.HTTPError:
        pass
    call("PUT", f"/{INDEX}", {
        "mappings": {"properties": {
            "@timestamp": {"type": "date"},
            "event":      {"type": "keyword"},
            "src_ip":     {"type": "keyword"},
            "user":       {"type": "keyword"},
        }}
    })
    batch, n = [], 0
    with LOGS.open() as f:
        for line in f:
            ev = json.loads(line)
            # keep only auth outcomes; drop the free-text message for speed
            if ev.get("event") not in {"failed_password", "invalid_user", "auth_failure"}:
                continue
            batch.append(json.dumps({"index": {"_index": INDEX}}))
            batch.append(json.dumps({k: ev.get(k) for k in ("@timestamp", "event", "src_ip", "user")}))
            if len(batch) >= 2 * BATCH:
                call("POST", "/_bulk", "\n".join(batch) + "\n")
                n += BATCH
                batch = []
    if batch:
        call("POST", "/_bulk?refresh=true", "\n".join(batch) + "\n")
        n += len(batch) // 2
    else:
        call("POST", f"/{INDEX}/_refresh")
    print(f"indexed {call('GET', f'/{INDEX}/_count')['count']} real failed-auth events\n")


def detect():
    # A detector is just: which index, which clock field, what metric,
    # what bucket size, and how many stddevs count as anomalous.
    call("PUT", f"/_ml/anomaly_detectors/{DETECTOR}", {
        "source_index": INDEX,
        "time_field": "@timestamp",
        "function": "count",
        "bucket_span": "1h",
        "anomaly_threshold": 3.0,
        "description": "failed ssh auth volume vs moving baseline",
    })
    result = call("POST", f"/_ml/anomaly_detectors/{DETECTOR}/_score")
    records = [r for r in result.get("records", []) if r.get("is_anomaly")]
    records.sort(key=lambda r: -r["anomaly_score"])

    print(f"{len(records)} anomalous hours found (threshold: 3.0σ over moving baseline)\n")
    print("  score   when                 failed auths   expected")
    for r in records[:10]:
        ts = datetime.datetime.fromtimestamp(r["timestamp"] / 1000, datetime.timezone.utc)
        print(f"  {r['anomaly_score']:5.1f}   {ts:%Y-%m-%d %H:%M} UTC   {r['actual']:>10.0f}   {r['expected']:>8.1f}")

    if records:
        # Who caused the worst hour? Ordinary ES aggregation on the bucket.
        worst = records[0]
        start = worst["timestamp"]
        top = call("POST", f"/{INDEX}/_search", {
            "size": 0,
            "query": {"range": {"@timestamp": {"gte": start, "lt": start + 3_600_000}}},
            "aggs": {"attackers": {"terms": {"field": "src_ip", "size": 3}}},
        })["aggregations"]["attackers"]["buckets"]
        print("\nworst hour, top attacking IPs:")
        for b in top:
            print(f"  {b['key']:<18} {b['doc_count']} attempts")


if __name__ == "__main__":
    ingest()
    detect()
