#!/usr/bin/env python3
"""Benchmark: full-text search over one JSONL file, four local tools.
Method (published on xerj.org/compare/best-local-full-text-search-jsonl-csv-2026):
- corpus: gen_corpus.py, 300,000 records, 58.3 MB, seed 20261019
- index time: wall clock from the raw file to a queryable index
- query latency: median of 20 runs after 3 warm-up runs, counting matches of
  3 queries on the `msg` field (ripgrep searches the whole line)
"""
import json, os, statistics, subprocess, sys, time, urllib.request, sqlite3, shutil
HERE = os.path.dirname(os.path.abspath(__file__))
CORPUS = os.path.abspath(os.environ.get("CORPUS", os.path.join(HERE, "corpus/logs.jsonl")))
WORK = os.path.join(HERE, "work"); os.makedirs(WORK, exist_ok=True)
Q = [("rare term", "kestrel"), ("common term", "timeout"), ("phrase", "disk quota exceeded")]
res = {}
def lat(fn, n=20, warm=3):
    for _ in range(warm): fn()
    ts = []
    for _ in range(n):
        t = time.perf_counter(); out = fn(); ts.append((time.perf_counter() - t) * 1000)
    return round(statistics.median(ts), 2), out
def du(p):
    if os.path.isfile(p): return os.path.getsize(p)
    return sum(os.path.getsize(os.path.join(d, f)) for d, _, fs in os.walk(p) for f in fs)

def bench_rg():
    r = {"tool": "ripgrep " + subprocess.run(["rg", "--version"], capture_output=True, text=True).stdout.split()[1],
         "index_s": 0.0, "index_bytes": 0, "queries": {}}
    for name, q in Q:
        args = ["rg", "-c", "-F", q, CORPUS] if " " in q else ["rg", "-c", "-w", q, CORPUS]
        ms, out = lat(lambda: subprocess.run(args, capture_output=True, text=True).stdout.strip())
        r["queries"][name] = {"ms": ms, "hits": int(out)}
    return r

def bench_sqlite():
    db = os.path.join(WORK, "fts5.db")
    if os.path.exists(db): os.remove(db)
    t = time.perf_counter()
    c = sqlite3.connect(db)
    c.execute("CREATE VIRTUAL TABLE logs USING fts5(ts UNINDEXED, level, service, user, msg)")
    with open(CORPUS) as f:
        c.executemany("INSERT INTO logs VALUES (?,?,?,?,?)",
                      ((d["ts"], d["level"], d["service"], d["user"], d["msg"]) for d in map(json.loads, f)))
    c.commit()
    idx = time.perf_counter() - t
    r = {"tool": "SQLite FTS5 " + sqlite3.sqlite_version, "index_s": round(idx, 2), "index_bytes": du(db), "queries": {}}
    for name, q in Q:
        expr = 'msg:"%s"' % q
        ms, out = lat(lambda: c.execute("SELECT count(*) FROM logs WHERE logs MATCH ?", (expr,)).fetchone()[0])
        r["queries"][name] = {"ms": ms, "hits": out}
    return r

def bench_duckdb():
    # A Python with `duckdb` installed (pip install duckdb); defaults to this one.
    py = os.environ.get("DUCKDB_PYTHON", sys.executable)
    out = subprocess.run([py, os.path.join(HERE, "duck.py"), CORPUS, WORK], capture_output=True, text=True)
    if out.returncode: return {"tool": "DuckDB", "error": out.stderr[-400:]}
    return json.loads(out.stdout)

def http(method, url, body=None):
    req = urllib.request.Request(url, data=json.dumps(body).encode() if body else None, method=method,
                                 headers={"content-type": "application/json"})
    with urllib.request.urlopen(req) as resp: return json.loads(resp.read())

def bench_xerj():
    xerj = os.environ.get("XERJ", "xerj"); data = os.path.join(WORK, "xerj-data"); state = os.path.join(WORK, "xerj-state")
    for p in (data, state): shutil.rmtree(p, ignore_errors=True)
    src = os.path.join(WORK, "xerj-src"); shutil.rmtree(src, ignore_errors=True); os.makedirs(src)
    link = os.path.join(src, "logs.jsonl")
    if not os.path.exists(link): os.link(CORPUS, link)  # autoindex does not follow symlinks
    node = subprocess.Popen([xerj, "--insecure", "--data-dir", data, "--port", "9277"],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(200):
            try: http("GET", "http://127.0.0.1:9277/_cluster/health"); break
            except Exception: time.sleep(0.1)
        t = time.perf_counter()
        log = open(os.path.join(WORK, "autoindex.stderr.txt"), "w")
        ai = subprocess.Popen([xerj, "autoindex", src, "--url", "http://127.0.0.1:9277", "--prefix", "bx",
                               "--state-dir", state, "--no-graph", "--progress", "plain"],
                              stdout=subprocess.DEVNULL, stderr=log, text=True)
        peak = 0
        while ai.poll() is None:
            try:
                with open(f"/proc/{node.pid}/status") as st:
                    peak = max(peak, next(int(l.split()[1]) for l in st if l.startswith("VmHWM")))
            except (FileNotFoundError, StopIteration): pass
            time.sleep(0.5)
        idx = time.perf_counter() - t; log.close()
        p = subprocess.CompletedProcess(ai.args, ai.returncode, "", open(log.name).read())
        if node.poll() is not None:
            return {"tool": "xerj", "index_s": round(idx, 2), "node_exit": node.returncode,
                    "node_peak_rss_kb": peak, "autoindex_exit": ai.returncode,
                    "autoindex_tail": p.stderr.strip().splitlines()[-3:], "error": "node exited during indexing"}
        http("POST", "http://127.0.0.1:9277/bx-*/_refresh")
        version = subprocess.run([xerj, "--version"], capture_output=True, text=True).stdout.split()[1]
        r = {"tool": "xerj " + version, "index_s": round(idx, 2), "index_bytes": du(data), "autoindex_exit": p.returncode, "node_peak_rss_kb_during_index": peak,
             "autoindex_tail": p.stderr.strip().splitlines()[-1:] , "queries": {}}
        for name, q in Q:
            body = {"query": {"match_phrase": {"msg": q}}}
            ms, out = lat(lambda: http("POST", "http://127.0.0.1:9277/bx-*/_count", body)["count"])
            r["queries"][name] = {"ms": ms, "hits": out}
        return r
    finally:
        node.terminate(); node.wait()

if __name__ == "__main__":
    which = sys.argv[1:] or ["rg", "sqlite", "duckdb", "xerj"]
    env = {"tool_versions": {}, "host": subprocess.run(["uname", "-srm"], capture_output=True, text=True).stdout.strip(),
           "cpus": os.cpu_count(), "corpus_bytes": os.path.getsize(CORPUS), "results": []}
    for w in which:
        env["results"].append(globals()["bench_" + w]()); print(json.dumps(env["results"][-1]), file=sys.stderr)
    print(json.dumps(env, indent=1))
