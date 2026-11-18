import duckdb, json, os, statistics, sys, time
corpus, work = sys.argv[1], sys.argv[2]
db = os.path.join(work, "fts.duckdb")
if os.path.exists(db): os.remove(db)
t = time.perf_counter()
c = duckdb.connect(db); c.execute("LOAD fts")
c.execute(f"CREATE TABLE logs AS SELECT row_number() OVER () AS id, * FROM read_json_auto('{corpus}')")
c.execute("PRAGMA create_fts_index('logs', 'id', 'msg', stemmer='none', stopwords='none')")
c.execute("CHECKPOINT")
idx = time.perf_counter() - t
r = {"tool": "DuckDB FTS " + duckdb.__version__, "index_s": round(idx, 2), "index_bytes": os.path.getsize(db), "queries": {}}
Q = [("rare term", "kestrel", 0), ("common term", "timeout", 0), ("phrase", "disk quota exceeded", 1)]
for name, q, conj in Q:
    # match_bm25 has no phrase operator; conjunctive=1 requires every term (a superset of the phrase).
    sql = f"SELECT count(*) FROM (SELECT fts_main_logs.match_bm25(id, ?, fields := 'msg', conjunctive := {conj}) AS s FROM logs) WHERE s IS NOT NULL"
    f = lambda: c.execute(sql, [q]).fetchone()[0]
    for _ in range(3): f()
    ts = []
    for _ in range(20):
        t = time.perf_counter(); out = f(); ts.append((time.perf_counter() - t) * 1000)
    r["queries"][name] = {"ms": round(statistics.median(ts), 2), "hits": out}
print(json.dumps(r))
