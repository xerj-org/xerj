"""Issue #1091: per-query wall time of the semantic and hybrid first stages.

Runs the first N BEIR queries (in file order) against an index loaded by
load.py, once each, cache-off, as a `semantic` query and as the same `hybrid`
(BM25 + semantic, RRF) body judge_gate.py sends. Writes every query's wall
time and returned ids to a JSON file so two builds can be compared both for
speed and for identical answers.

    python3 latency_1091.py run  <url> <index> <queries.jsonl> <n> <out.json> <expected_docs>
    python3 latency_1091.py diff <before.json> <after.json>
"""
import json
import sys
import time
import urllib.request


def post(url, index, body):
    req = urllib.request.Request(
        f"{url}/{index}/_search?request_cache=false",
        data=json.dumps(body).encode(),
        headers={"content-type": "application/json"},
    )
    started = time.time()
    res = json.loads(urllib.request.urlopen(req, timeout=600).read())
    return (time.time() - started) * 1000.0, [h["_id"] for h in res["hits"]["hits"]]


def bm25(q):
    return {"multi_match": {"query": q, "fields": ["title", "text"]}}


def sem(q):
    return {"semantic": {"field": "body", "query": q}}


ARMS = {
    "semantic": lambda q: {"size": 30, "query": sem(q)},
    "hybrid": lambda q: {
        "size": 30,
        "query": {"hybrid": {"queries": [{"query": bm25(q)}, {"query": sem(q)}], "fusion": "rrf"}},
    },
}


def pct(values, p):
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(round(p * (len(ordered) - 1))))]


def run(url, index, queries_path, n, out_path, expected_docs):
    count = json.loads(urllib.request.urlopen(f"{url}/{index}/_count").read())["count"]
    if count != expected_docs:
        sys.exit(f"{index} holds {count} documents, expected {expected_docs}: reload before measuring")
    queries = [json.loads(line)["text"] for line in open(queries_path)][:n]
    out = {}
    for arm, body in ARMS.items():
        rows = []
        for q in queries:
            wall, ids = post(url, index, body(q))
            rows.append({"q": q, "wall_ms": wall, "ids": ids})
        walls = [r["wall_ms"] for r in rows]
        # The first request of the first arm pays any cold per-segment work.
        print(
            f"{arm:9s} n={len(rows)} first={walls[0]:.0f}ms "
            f"p50={pct(walls[1:], 0.5):.0f}ms p95={pct(walls[1:], 0.95):.0f}ms "
            f"max={max(walls[1:]):.0f}ms (p50/p95/max exclude the first)"
        )
        out[arm] = rows
    json.dump(out, open(out_path, "w"), indent=1)


def diff(before_path, after_path):
    before, after = json.load(open(before_path)), json.load(open(after_path))
    for arm in ARMS:
        pairs = list(zip(before[arm], after[arm]))
        same = sum(b["ids"] == a["ids"] for b, a in pairs)
        print(f"{arm:9s} identical ranked ids: {same}/{len(pairs)}")
        for b, a in pairs:
            if b["ids"] != a["ids"]:
                print(f"  differs: {b['q'][:60]!r}")


if __name__ == "__main__":
    if sys.argv[1] == "run":
        run(sys.argv[2], sys.argv[3], sys.argv[4], int(sys.argv[5]), sys.argv[6], int(sys.argv[7]))
    else:
        diff(sys.argv[2], sys.argv[3])
