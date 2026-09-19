#!/usr/bin/env python3
"""End-to-end `_bulk` ingest throughput into a `semantic_text` field, with the
server's own CPU use, so "how many cores did it keep busy" is a number.

    python3 ingest_bench.py http://localhost:9560 <server_pid> /path/to/beir/scifact 400 [--titles-only] [clients ...]

Runs one pass per `clients` value (default: 1 4 8): the same 400 documents,
50 per `_bulk`, sent by that many concurrent clients. `--titles-only` indexes
just each document's title - a one-passage, one-sentence document - to show the
short-document rate through the same path. Linux only (/proc).
WRITES to the node: it creates and deletes throwaway indices named tp_*.
"""
import concurrent.futures as cf
import json
import os
import sys
import time

from common import http, process_cpu_seconds

url, pid, data_dir, n = sys.argv[1], int(sys.argv[2]), sys.argv[3], int(sys.argv[4])
rest = sys.argv[5:]
titles_only = "--titles-only" in rest
client_counts = [int(c) for c in rest if c != "--titles-only"] or [1, 4, 8]
docs = [json.loads(l) for l in open(data_dir + "/corpus.jsonl")][:n]
docs = [d["title"] if titles_only else d["title"] + ". " + d["text"] for d in docs]
BULK = 50
print(f"docs={len(docs)} mean_chars={sum(map(len, docs)) / len(docs):.0f} cores={os.cpu_count()}")


def bulk(name, part, off):
    lines = []
    for j, d in enumerate(part):
        lines.append(json.dumps({"index": {"_index": name, "_id": str(off + j)}}))
        lines.append(json.dumps({"text": d, "body": d}))
    r = http(url, "POST", "/_bulk", ("\n".join(lines) + "\n").encode(), "application/x-ndjson")
    return bool(r.get("errors") or "_err" in r), str(r)[:200]


for clients in client_counts:
    name = f"tp_clients{clients}"
    http(url, "DELETE", "/" + name)
    http(url, "PUT", "/" + name, {"mappings": {"properties": {
        "text": {"type": "text"}, "body": {"type": "semantic_text"}}}})
    parts = [(docs[i:i + BULK], i) for i in range(0, len(docs), BULK)]
    load = open("/proc/loadavg").read().split()[0]
    c0, t0 = process_cpu_seconds(pid), time.time()
    with cf.ThreadPoolExecutor(clients) as ex:
        res = list(ex.map(lambda a: bulk(name, *a), parts))
    wall, used = time.time() - t0, process_cpu_seconds(pid) - c0
    errs = [r for r in res if r[0]]
    http(url, "POST", f"/{name}/_refresh")
    count = http(url, "GET", f"/{name}/_count").get("count")
    print(f"loadavg={load} clients={clients} bulk={BULK} docs={len(docs)} indexed={count} "
          f"errors={len(errs)} wall={wall:6.1f}s docs/s={len(docs) / wall:6.1f} "
          f"server_cpu_s={used:7.1f} avg_cores_busy={used / wall:5.1f}", flush=True)
    if errs:
        print("  first error:", errs[0][1])
    http(url, "DELETE", "/" + name)
