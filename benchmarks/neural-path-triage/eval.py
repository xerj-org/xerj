#!/usr/bin/env python3
"""nDCG@10 on a BEIR test split for four retrieval arms on a live XERJ node.

    python3 eval.py http://localhost:9560 scifact /path/to/beir/scifact [rerank_window=30]

Every number printed comes from a query against the node; nothing is assumed.
Read-only: it only sends `_search` and `_count`.

Latency columns are only meaningful on the FIRST pass over a query set: a
repeated identical request is answered from the node's result cache in under a
millisecond (see README, "cache mirage").
"""
import sys
import time

from common import bm25_query, http, hybrid_query, load_beir, ndcg10, semantic_query

url, index, data_dir = sys.argv[1], sys.argv[2], sys.argv[3]
window = int(sys.argv[4]) if len(sys.argv) > 4 else 30
queries, qrels = load_beir(data_dir)


def ids(res):
    return [h["_id"] for h in res.get("hits", {}).get("hits", [])]


def search(query, n=100):
    return http(url, "POST", f"/{index}/_search", {"size": n, "_source": False, "query": query})


def rerank_window(q, w):
    """BM25 top-w reordered by the embedding score; the tail keeps BM25 order."""
    base = ids(search(bm25_query(q)))
    win = base[:w]
    r = search({"bool": {"must": [semantic_query(q)], "filter": [{"ids": {"values": win}}]}}, w)
    if "_err" in r:
        raise SystemExit(f"filtered semantic failed: {r}")
    order = ids(r)
    seen = set(order)
    return order + [d for d in win if d not in seen] + base[w:]


arms = {
    "bm25": lambda q: ids(search(bm25_query(q))),
    "minilm (vector only)": lambda q: ids(search(semantic_query(q))),
    "hybrid rrf (server)": lambda q: ids(search(hybrid_query(q))),
    f"bm25 top-{window} -> minilm rerank": lambda q: rerank_window(q, window),
}
probe = search(semantic_query("test"), 1)
if "_err" in probe:
    raise SystemExit(f"semantic arm unavailable (is the node running --embed-mode neural?): {probe}")
print(f"queries={len(qrels)}  docs={http(url, 'GET', f'/{index}/_count').get('count')}")
for name, fn in arms.items():
    scores, lat, empty = [], [], 0
    for qid, rel in qrels.items():
        t0 = time.time()
        ranked = fn(queries[qid])
        lat.append(time.time() - t0)
        empty += not ranked
        scores.append(ndcg10(ranked, rel))
    lat.sort()
    print(f"{name:34s} nDCG@10={sum(scores) / len(scores):.4f}  empty={empty:3d}  "
          f"p50={lat[len(lat) // 2] * 1000:6.1f}ms  p95={lat[int(len(lat) * .95)] * 1000:6.1f}ms")
