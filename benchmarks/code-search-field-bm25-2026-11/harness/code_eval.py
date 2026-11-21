"""usage: code_eval.py <xerj> <url> <corpus> <queries.tsv> <qrels.tsv> <out.json> [k]

Runs every judged query through `xerj code <corpus> <q> --json -k <k>` (the
default bm25 mode) and scores the distinct `_source.ax_path` list with the
same definitions `xerj eval` uses: nDCG@k with linear gain, recall@k,
MRR@k; a grade > 0 is relevant; a repeated path keeps its first rank. It
also records the wall time per call and the node's `took` per query.

XERJ_CODE_HOME must point at the code home that holds the corpus ledger.
Per-set means are keyed on the first character of the qid (A/B/C for the
search-exact-phrase sets, `q` for the Lucene questions)."""
import collections
import json
import math
import subprocess
import sys
import time

X, URL, CORPUS, QF, RF, OUT = sys.argv[1:7]
K = int(sys.argv[7]) if len(sys.argv) > 7 else 10

queries = [l.rstrip("\n").split("\t", 1) for l in open(QF, encoding="utf-8") if l.strip()]
qrels = collections.defaultdict(dict)
for line in open(RF, encoding="utf-8"):
    parts = line.rstrip("\n").split("\t")
    if len(parts) != 3 or parts[0] == "query-id":
        continue
    qrels[parts[0]][parts[1]] = int(parts[2])


def score(ranked, judged):
    top = []
    for d in ranked:
        if d not in top:
            top.append(d)
        if len(top) == K:
            break
    dcg = sum(judged.get(d, 0) / math.log2(i + 2) for i, d in enumerate(top))
    ideal = sorted((g for g in judged.values() if g > 0), reverse=True)[:K]
    idcg = sum(g / math.log2(i + 2) for i, g in enumerate(ideal))
    rel = {d for d, g in judged.items() if g > 0}
    hit = [i for i, d in enumerate(top) if d in rel]
    return {
        "ndcg": dcg / idcg if idcg else 0.0,
        "recall": len(set(top) & rel) / len(rel),
        "mrr": 1.0 / (hit[0] + 1) if hit else 0.0,
        "rank": hit[0] + 1 if hit else None,
        "retrieved": top,
    }


rows = []
for qid, text in queries:
    if qid not in qrels:
        continue
    t = time.time()
    r = subprocess.run(
        [X, "code", CORPUS, text, "--json", "-k", str(K), "--url", URL],
        capture_output=True,
        text=True,
    )
    ms = (time.time() - t) * 1000
    paths, err, took = [], None, None
    try:
        resp = json.loads(r.stdout)
        took = resp.get("took")
        for h in resp["hits"]["hits"]:
            p = (h.get("_source") or {}).get("ax_path")
            if p:
                paths.append(p)
    except Exception:
        err = (r.stdout + r.stderr)[-300:]
    m = score(paths, qrels[qid])
    rows.append({"qid": qid, "q": text, "latency_ms": round(ms, 1), "took_ms": took, "err": err, **m})

by = collections.defaultdict(list)
for row in rows:
    by[row["qid"][0]].append(row)
by["all"] = rows


def pct(v, p):
    v = sorted(v)
    return v[min(len(v) - 1, int(p * len(v)))] if v else None


def means(rs):
    n = len(rs)
    return {
        "n": n,
        "ndcg": sum(r["ndcg"] for r in rs) / n,
        "recall": sum(r["recall"] for r in rs) / n,
        "mrr": sum(r["mrr"] for r in rs) / n,
        "r1": sum(r["rank"] == 1 for r in rs),
        "top5": sum(bool(r["rank"] and r["rank"] <= 5) for r in rs),
        "top10": sum(bool(r["rank"]) for r in rs),
        "errors": sum(bool(r["err"]) for r in rs),
        "latency_p50_ms": sorted(r["latency_ms"] for r in rs)[n // 2],
        "took_p50_ms": pct([r["took_ms"] for r in rs if r["took_ms"] is not None], 0.5),
        "took_p95_ms": pct([r["took_ms"] for r in rs if r["took_ms"] is not None], 0.95),
    }


report = {"k": K, "corpus": CORPUS, "sets": {s: means(rs) for s, rs in sorted(by.items())}, "queries": rows}
json.dump(report, open(OUT, "w"), indent=1)
for s, m in report["sets"].items():
    print(s, json.dumps({k: round(v, 4) if isinstance(v, float) else v for k, v in m.items()}))
