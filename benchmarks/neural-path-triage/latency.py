#!/usr/bin/env python3
"""Per-arm search latency on queries the node has NOT seen (offset into queries.jsonl).

    python3 latency.py http://localhost:9560 scifact /path/to/beir/scifact 60 300

Arguments: url index data_dir n_queries offset. Pick an offset you have not
used against this process before: an identical request is answered from the
result cache in <1 ms, which is what the last arm demonstrates on purpose.
Start the node with XERJ_TRACE_SEMANTIC_PHASES=1 and feed its log to phases.py
to split each request into embed / collect / score / top-k.
Read-only: `_search` only.
"""
import json
import sys
import time

from common import bm25_query, http, hybrid_query, semantic_query

url, index, data_dir, n, off = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4]), int(sys.argv[5])
qs = [json.loads(l)["text"] for l in open(data_dir + "/queries.jsonl")][off:off + n]


def search(query, size=10):
    return http(url, "POST", f"/{index}/_search", {"size": size, "_source": False, "query": query})


def filtered(q):
    win = [h["_id"] for h in search(bm25_query(q), 30)["hits"]["hits"]]
    t = time.perf_counter()
    r = search({"bool": {"must": [semantic_query(q)], "filter": [{"ids": {"values": win}}]}})
    return time.perf_counter() - t, r


def timed(query):
    t = time.perf_counter()
    r = search(query)
    return time.perf_counter() - t, r


arms = [
    ("bm25", lambda q: timed(bm25_query(q))),
    ("semantic (unfiltered)", lambda q: timed(semantic_query(q))),
    ("semantic + ids filter (30 docs)", filtered),
    ("hybrid rrf", lambda q: timed(hybrid_query(q))),
    ("semantic, SAME query repeated", lambda q: timed(semantic_query(qs[0]))),
]
for name, fn in arms:
    print("ARM_START", name, time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime()), flush=True)
    lat, took = [], []
    for q in qs:
        dt, r = fn(q)
        lat.append(dt * 1000)
        took.append(r.get("took", 0))
    lat.sort()
    print(f"{name:34s} n={len(lat)} p50={lat[len(lat) // 2]:7.1f}ms p95={lat[int(len(lat) * .95)]:7.1f}ms"
          f"  server took p50={sorted(took)[len(took) // 2]}ms", flush=True)
    print("ARM_END", name, time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime()), flush=True)
