#!/usr/bin/env python3
"""Live Elasticsearch/Lucene floor for the autoindex release gate.

  es_baseline.py --es http://127.0.0.1:9200 --corpus DIR --queries DIR.queries.json \
                 --out es-baseline.json [--workers 4] [--seed 2421]

Bulk-loads the SAME corpus produced by gen_corpus.py into one ES index with
default dynamic mappings (one doc per line for .jsonl/.csv/.log/.ndjson,
one doc per file otherwise, ES default dynamic mapping), refreshes + flushes
(ingest time stops here), force-merges to ONE segment (index_bytes is the
merged size), then runs the corpus' search queries uncached. Writes the JSON that
`gate.py eval --engine-baseline` consumes:

  {"engine", "corpus_mb", "seed", "source_bytes", "docs", "index_bytes",
   "ingest_mb_s", "search_p95_ms", "wall_s"}

ingest_mb_s is source bytes / wall time from first bulk to refreshed+flushed
index, the same definition gate.py uses for xerj (bytes / autoindex wall).
stdlib only; the ES host is assumed throwaway (the index is deleted first).
"""
import argparse, json, os, queue, threading, time, urllib.request

MiB = 1024 * 1024
LINE_EXT = {".jsonl", ".ndjson", ".csv", ".log"}


def http(method, url, body=None, ctype="application/json", timeout=300):
    data = body if isinstance(body, (bytes, type(None))) else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method, headers={"Content-Type": ctype})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read() or b"{}")


def docs(corpus):
    for root, _, files in os.walk(corpus):
        for f in sorted(files):
            p = os.path.join(root, f)
            rel = os.path.relpath(p, corpus)
            ext = os.path.splitext(f)[1].lower()
            try:
                text = open(p, encoding="utf-8", errors="replace").read()
            except OSError:
                continue
            if ext in LINE_EXT:
                lines = text.splitlines()
                header = lines[0] if ext == ".csv" and lines else None
                for i, ln in enumerate(lines):
                    if ln.strip():
                        d = {"path": rel, "line": i, "body": ln}
                        if header and i:
                            d["header"] = header
                        yield d
            else:
                yield {"path": rel, "body": text}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--es", default=os.environ.get("ES_URL", "http://127.0.0.1:9200"))
    ap.add_argument("--corpus", required=True); ap.add_argument("--queries", required=True)
    ap.add_argument("--out", required=True); ap.add_argument("--index", default="perf-floor")
    ap.add_argument("--workers", type=int, default=4); ap.add_argument("--bulk-mb", type=float, default=5)
    ap.add_argument("--seed", type=int, default=2421)
    a = ap.parse_args()
    info = http("GET", a.es + "/")
    engine = "elasticsearch-%s (lucene %s)" % (info["version"]["number"], info["version"].get("lucene_version"))
    try:
        http("DELETE", "%s/%s" % (a.es, a.index))
    except Exception:
        pass
    http("PUT", "%s/%s" % (a.es, a.index), {"settings": {"number_of_shards": 1, "number_of_replicas": 0, "refresh_interval": "-1"}})  # default dynamic mapping, as the floor was measured
    src = sum(os.path.getsize(os.path.join(r, f)) for r, _, fs in os.walk(a.corpus) for f in fs)
    q = queue.Queue(maxsize=a.workers * 2)
    errs = []

    def worker():
        while True:
            b = q.get()
            if b is None:
                return
            last = None
            for attempt in range(5):
                try:
                    r = http("POST", "%s/%s/_bulk" % (a.es, a.index), b, "application/x-ndjson")
                    if r.get("errors"):
                        errs.append(next(i for i in r["items"] if i["index"].get("error"))["index"]["error"])
                    break
                except Exception as e:  # 429 / transient
                    last = e
                    time.sleep(2 ** attempt)
            else:
                errs.append(str(last))

    th = [threading.Thread(target=worker, daemon=True) for _ in range(a.workers)]
    [t.start() for t in th]
    t0 = time.time(); buf = []; size = 0; n = 0
    for d in docs(a.corpus):
        line = b'{"index":{}}\n' + json.dumps(d).encode() + b"\n"
        buf.append(line); size += len(line); n += 1
        if size >= a.bulk_mb * MiB:
            q.put(b"".join(buf)); buf = []; size = 0
    if buf:
        q.put(b"".join(buf))
    for _ in th:
        q.put(None)
    [t.join() for t in th]
    http("POST", "%s/%s/_refresh" % (a.es, a.index)); http("POST", "%s/%s/_flush" % (a.es, a.index))
    wall = time.time() - t0
    unmerged = http("GET", "%s/%s/_stats/store" % (a.es, a.index))["_all"]["primaries"]["store"]["size_in_bytes"]
    # index_bytes is defined AFTER a force-merge to one segment: the refreshed
    # size swings with the segment count (1.77x vs 1.42x source on 1 GB).
    t1 = time.time()
    http("POST", "%s/%s/_forcemerge?max_num_segments=1&flush=true" % (a.es, a.index), timeout=3600)
    fm_s = time.time() - t1
    st = http("GET", "%s/%s/_stats/store,docs" % (a.es, a.index))["_all"]["primaries"]
    qs = json.load(open(a.queries))
    allq = [x for k in ("cli", "raw", "mcp") for x in qs.get(k, [])]
    lat = []
    for s in allq:
        t1 = time.time()
        http("POST", "%s/%s/_search?request_cache=false" % (a.es, a.index),
             {"size": 5, "query": {"simple_query_string": {"query": s, "fields": ["*"], "lenient": True}}})
        lat.append((time.time() - t1) * 1000)
    lat.sort()
    p95 = lat[min(len(lat) - 1, int(round(0.95 * (len(lat) - 1))))] if lat else None
    res = {"engine": engine, "corpus_mb": round(src / MiB), "seed": a.seed, "source_bytes": src,
           "docs": st["docs"]["count"], "index_bytes": st["store"]["size_in_bytes"],
           "index_bytes_refreshed_unmerged": unmerged, "forcemerge_s": round(fm_s, 1),
           "ingest_mb_s": round(src / MiB / wall, 2), "search_p95_ms": round(p95, 1) if p95 else None,
           "wall_s": round(wall, 1), "bulk_errors": len(errs), "first_error": errs[0] if errs else None}
    json.dump(res, open(a.out, "w"), indent=1)
    print(json.dumps(res))


if __name__ == "__main__":
    main()
