#!/usr/bin/env python3
"""Is the hybrid RRF order a function of the index, or of the process?

    python3 rrf_stability.py save    http://localhost:9560 scifact /path/to/beir/scifact 40 before.json
    # ... restart the node on the SAME data directory ...
    python3 rrf_stability.py save    http://localhost:9560 scifact /path/to/beir/scifact 40 after.json
    python3 rrf_stability.py compare before.json after.json

Read-only: `_search` only.
"""
import json
import sys

from common import http, hybrid_query, load_beir

if sys.argv[1] == "save":
    url, index, data_dir, n, out = sys.argv[2], sys.argv[3], sys.argv[4], int(sys.argv[5]), sys.argv[6]
    queries, qrels = load_beir(data_dir)
    res = {}
    for qid in list(qrels)[:n]:
        r = http(url, "POST", f"/{index}/_search?request_cache=false",
                 {"size": 100, "_source": False, "query": hybrid_query(queries[qid])})
        res[qid] = [(h["_id"], h["_score"]) for h in r["hits"]["hits"]]
    json.dump(res, open(out, "w"))
    print("saved", len(res), "result lists to", out)
else:
    a, b = json.load(open(sys.argv[2])), json.load(open(sys.argv[3]))
    diff = [q for q in a if [x[0] for x in a[q]] != [x[0] for x in b[q]]]
    top10 = [q for q in a if [x[0] for x in a[q][:10]] != [x[0] for x in b[q][:10]]]
    same_set = all(set(x[0] for x in a[q]) == set(x[0] for x in b[q]) for q in a)
    print(f"queries={len(a)}  order differs={len(diff)}  top-10 differs={len(top10)}  same hit sets={same_set}")
    for q in diff[:5]:
        i = next(i for i, (x, y) in enumerate(zip(a[q], b[q])) if x[0] != y[0])
        print(f"  query {q}: first difference at rank {i + 1}: {a[q][i:i + 2]}  ->  {b[q][i:i + 2]}")
