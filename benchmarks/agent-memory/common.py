"""Shared loop for every adapter. Stdlib only.

An adapter supplies add(memory) and search(question, k) -> list[str] (ranked
memory texts). run() writes results/<system>__<config>.json with the raw ranked
texts for every question, per-call latencies and environment info.
score.py turns those raw files into metrics, the same way for every system.
"""
import json, os, platform, sys, time, statistics, datetime

HERE = os.path.dirname(os.path.abspath(__file__))
K = 5

def load():
    return json.load(open(os.path.join(HERE, "dataset.json")))

def run(system, config, add, search, meta, k=K, settle=None):
    ds = load()
    write_ms = []
    t_all = time.perf_counter()
    for mem in ds["memories"]:
        t = time.perf_counter()
        add(mem)
        write_ms.append((time.perf_counter() - t) * 1000)
    if settle:
        settle()
    ingest_s = time.perf_counter() - t_all
    out = []
    query_ms = []
    for qq in ds["questions"]:
        t = time.perf_counter()
        hits = search(qq["question"], k)
        dt = (time.perf_counter() - t) * 1000
        query_ms.append(dt)
        out.append({"id": qq["id"], "hits": [h for h in hits][:k], "ms": round(dt, 2)})
    res = {
        "system": system, "config": config, "k": k,
        "run_at": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "host": {"python": platform.python_version(), "machine": platform.machine(),
                 "cpus": os.cpu_count()},
        "meta": meta,
        "n_memories": len(ds["memories"]), "n_questions": len(ds["questions"]),
        "write_ms": [round(x, 2) for x in write_ms],
        "ingest_total_s": round(ingest_s, 2),
        "questions": out,
    }
    os.makedirs(os.path.join(HERE, "results"), exist_ok=True)
    p = os.path.join(HERE, "results", f"{system}__{config}.json")
    json.dump(res, open(p, "w"), indent=1)
    print("wrote", p, "write p50 ms", round(statistics.median(write_ms), 2),
          "query p50 ms", round(statistics.median(query_ms), 2))
    return res
