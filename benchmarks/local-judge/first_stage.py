"""First-stage candidate lists from a live XERJ node: BM25 top-100 and hybrid RRF top-100.
usage: first_stage.py <dataset_dir> <index> <split> <out.json> [max_queries] [seed]
Every list is a live `_search`; nothing is cached or assumed.
ARMS=hybrid (environment) runs only that arm: how the hybrid repeats for issue #940 are taken."""
import json, os, random, sys, time
from beirlib import call, queries, qrels

ds, idx, split, out = sys.argv[1:5]
limit = int(sys.argv[5]) if len(sys.argv) > 5 else 0
seed = int(sys.argv[6]) if len(sys.argv) > 6 else 13
Q, R = queries(ds), qrels(ds, split)
qids = sorted(R)
if limit and limit < len(qids):
    random.Random(seed).shuffle(qids)
    qids = sorted(qids[:limit])

BM = lambda q: {"multi_match": {"query": q, "fields": ["title", "text"]}}
SEM = lambda q: {"semantic": {"field": "body", "query": q}}
HYB = lambda q: {"hybrid": {"queries": [{"query": BM(q)}, {"query": SEM(q)}], "fusion": "rrf"}}


def ids(res):
    if "_err" in res:
        raise SystemExit(f"search failed: {res}")
    return [h["_id"] for h in res["hits"]["hits"]]


run = {"dataset": ds.rstrip("/").split("/")[-1], "index": idx, "split": split, "queries": len(qids),
       "docs": call("GET", f"/{idx}/_count").get("count"), "bm25": {}, "hybrid": {}, "latency_ms": {}}
ARMS = os.environ.get("ARMS", "bm25,hybrid").split(",")
for arm, qf in (("bm25", BM), ("hybrid", HYB)):
    if arm not in ARMS:
        continue
    lat = []
    for n, qid in enumerate(qids):
        t = time.time()
        run[arm][qid] = ids(call("POST", f"/{idx}/_search", {"size": 100, "_source": False, "query": qf(Q[qid])}))
        lat.append((time.time() - t) * 1000)
        if (n + 1) % 200 == 0:
            print(f"{arm} {n + 1}/{len(qids)}", flush=True)
    lat.sort()
    run["latency_ms"][arm] = {"p50": lat[len(lat) // 2], "p95": lat[int(len(lat) * .95)]}
    print(arm, run["latency_ms"][arm], "empty:", sum(1 for v in run[arm].values() if not v), flush=True)
json.dump(run, open(out, "w"))
