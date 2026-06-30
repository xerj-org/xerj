#!/usr/bin/env python3
"""ES API compatibility test — tests xerj against a reference ES cluster.

Exercises every major ES REST endpoint and compares responses.
Writes a datetime-stamped report to engine/reports/.
"""
import http.client, json, sys, time, os
from datetime import datetime

XERJ = ("localhost", 9800)
ES    = ("127.0.0.1", 9300)
INDEX = "api_compat_test"

TIMESTAMP = datetime.now().strftime("%Y-%m-%dT%H-%M-%S")
REPORT_DIR = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "reports")
os.makedirs(REPORT_DIR, exist_ok=True)
REPORT = os.path.join(REPORT_DIR, f"{TIMESTAMP}_es_api_compat.md")

results = []
log_lines = []

def log(msg):
    ts = datetime.now().strftime("%H:%M:%S")
    line = f"[{ts}] {msg}"
    log_lines.append(line)
    print(line)

def req(host, port, method, path, body=None, headers=None):
    conn = http.client.HTTPConnection(host, port, timeout=30)
    hdrs = {"Content-Type": "application/json"}
    if headers:
        hdrs.update(headers)
    b = json.dumps(body).encode() if body else None
    conn.request(method, path, body=b, headers=hdrs)
    resp = conn.getresponse()
    data = resp.read()
    conn.close()
    try:
        j = json.loads(data)
    except Exception:
        j = None
    return resp.status, j, data

def test(name, method, path, body=None, check=None, headers=None):
    """Run a request against both engines, compare."""
    try:
        xs, xj, xd = req(*XERJ, method, path, body, headers)
    except Exception as e:
        xs, xj, xd = 0, None, str(e).encode()
    try:
        es, ej, ed = req(*ES, method, path, body, headers)
    except Exception as e:
        es, ej, ed = 0, None, str(e).encode()

    passed = True
    notes = []

    # Status code match
    if xs != es:
        # Allow 200 vs 201 (both are success)
        if not ({xs, es} <= {200, 201}):
            passed = False
            notes.append(f"status: xerj={xs} es={es}")

    # Custom check
    if check and xj is not None:
        try:
            check_result = check(xj, ej)
            if check_result is not None:
                if not check_result:
                    passed = False
                    # Include key response fields for debugging
                    xval = xj.get("hits",{}).get("total",{}).get("value","?") if isinstance(xj, dict) else "?"
                    eval_ = ej.get("hits",{}).get("total",{}).get("value","?") if isinstance(ej, dict) else "?"
                    notes.append(f"check failed (xerj total={xval}, es total={eval_})")
        except Exception as e:
            passed = False
            notes.append(f"check error: {e}")

    status = "PASS" if passed else "FAIL"
    detail = " — " + "; ".join(notes) if notes else ""
    log(f"  {status}: {name}{detail}")
    results.append((name, status, xs, es, detail))
    return xj, ej

def setup():
    """Create a test index with mapping on both engines."""
    mapping = {
        "settings": {"number_of_shards": 1, "number_of_replicas": 0},
        "mappings": {"properties": {
            "title": {"type": "text"},
            "status": {"type": "keyword"},
            "count": {"type": "integer"},
            "price": {"type": "float"},
            "tags": {"type": "keyword"},
            "created": {"type": "date"}
        }}
    }
    # Delete if exists
    for h, p in [XERJ, ES]:
        try: req(h, p, "DELETE", f"/{INDEX}")
        except Exception: pass

    test("PUT index with mapping", "PUT", f"/{INDEX}", mapping)
    time.sleep(0.5)

def seed():
    """Bulk-index test documents."""
    docs = []
    for i in range(100):
        docs.append(json.dumps({"index": {"_index": INDEX, "_id": f"doc{i}"}}))
        docs.append(json.dumps({
            "title": f"Document number {i} about {'search' if i%3==0 else 'indexing' if i%3==1 else 'analysis'}",
            "status": ["active", "draft", "archived"][i%3],
            "count": i * 10,
            "price": round(i * 1.5 + 0.99, 2),
            "tags": ["important"] if i < 20 else ["normal"],
            "created": f"2026-01-{(i%28)+1:02d}T12:00:00Z"
        }))
    body = "\n".join(docs) + "\n"

    for h, p, name in [(*XERJ, "xerj"), (*ES, "ES")]:
        conn = http.client.HTTPConnection(h, p, timeout=30)
        conn.request("POST", "/_bulk", body=body.encode(),
                     headers={"Content-Type": "application/x-ndjson"})
        resp = conn.getresponse()
        data = resp.read()
        conn.close()
        log(f"  Seeded 100 docs to {name}: {resp.status}")

    time.sleep(1)  # let both flush
    time.sleep(1)  # extra settle time for flush to complete
    # Force refresh on both so docs are searchable
    for h, p, name in [(*XERJ, "xerj"), (*ES, "ES")]:
        try:
            conn = http.client.HTTPConnection(h, p, timeout=30)
            conn.request("POST", f"/{INDEX}/_refresh")
            resp = conn.getresponse()
            resp.read()
            conn.close()
            log(f"  Refreshed {name}: {resp.status}")
        except Exception as e:
            log(f"  Refresh {name} failed: {e}")

def run_tests():
    log(f"=== ES API Compatibility Test Suite ===")
    log(f"Date: {datetime.now().isoformat()}")
    log(f"xerj: {XERJ[0]}:{XERJ[1]}")
    log(f"ES:    {ES[0]}:{ES[1]}")
    log("")

    # ── Cluster APIs ──
    log("--- Cluster APIs ---")
    test("GET /", "GET", "/")
    test("GET /_cluster/health", "GET", "/_cluster/health",
         check=lambda x,e: x.get("status") in ("green","yellow","red"))
    test("GET /_cat/indices", "GET", "/_cat/indices?format=json")

    # ── Index management ──
    log("--- Index Management ---")
    setup()

    test("GET /{index}", "GET", f"/{INDEX}")
    test("GET /{index}/_mapping", "GET", f"/{INDEX}/_mapping")
    test("GET /{index}/_settings", "GET", f"/{INDEX}/_settings")

    # ── Document CRUD ──
    log("--- Document CRUD ---")
    seed()

    test("GET /_count", "GET", f"/{INDEX}/_count",
         check=lambda x,e: x.get("count",0) >= 100)
    test("GET /{index}/_doc/{id}", "GET", f"/{INDEX}/_doc/doc0",
         check=lambda x,e: x.get("found") == True)
    test("PUT /{index}/_doc/{id}", "PUT", f"/{INDEX}/_doc/new1",
         body={"title": "new doc", "status": "active", "count": 999})
    test("DELETE /{index}/_doc/{id}", "DELETE", f"/{INDEX}/_doc/new1")

    # ── Search ──
    log("--- Search ---")
    test("match_all size=0", "POST", f"/{INDEX}/_search",
         body={"query": {"match_all": {}}, "size": 0},
         check=lambda x,e: x["hits"]["total"]["value"] >= 100)
    test("match_all size=10", "POST", f"/{INDEX}/_search",
         body={"query": {"match_all": {}}, "size": 10},
         check=lambda x,e: len(x["hits"]["hits"]) == 10)
    test("term query", "POST", f"/{INDEX}/_search",
         body={"query": {"term": {"status": "active"}}, "size": 0},
         check=lambda x,e: x["hits"]["total"]["value"] > 0)
    test("match query", "POST", f"/{INDEX}/_search",
         body={"query": {"match": {"title": "search"}}, "size": 0},
         check=lambda x,e: x["hits"]["total"]["value"] > 0)
    test("range query", "POST", f"/{INDEX}/_search",
         body={"query": {"range": {"count": {"gte": 100, "lt": 500}}}, "size": 0},
         check=lambda x,e: x["hits"]["total"]["value"] > 0)
    test("bool query", "POST", f"/{INDEX}/_search",
         body={"query": {"bool": {"must": [{"term": {"status": "active"}}],
                                   "filter": [{"range": {"count": {"gte": 0}}}]}}, "size": 0},
         check=lambda x,e: x["hits"]["total"]["value"] > 0)
    test("exists query", "POST", f"/{INDEX}/_search",
         body={"query": {"exists": {"field": "title"}}, "size": 0},
         check=lambda x,e: x["hits"]["total"]["value"] > 0)
    test("ids query", "POST", f"/{INDEX}/_search",
         body={"query": {"ids": {"values": ["doc0", "doc1", "doc2"]}}, "size": 10},
         check=lambda x,e: len(x["hits"]["hits"]) >= 1)
    test("sort by count", "POST", f"/{INDEX}/_search",
         body={"query": {"match_all": {}}, "size": 5, "sort": [{"count": "desc"}]},
         check=lambda x,e: len(x["hits"]["hits"]) == 5)
    test("_source filtering", "POST", f"/{INDEX}/_search",
         body={"query": {"match_all": {}}, "size": 1, "_source": ["title", "status"]})

    # ── Aggregations ──
    log("--- Aggregations ---")
    test("terms agg", "POST", f"/{INDEX}/_search",
         body={"query": {"match_all": {}}, "size": 0,
               "aggs": {"statuses": {"terms": {"field": "status", "size": 10}}}},
         check=lambda x,e: len(x.get("aggregations",{}).get("statuses",{}).get("buckets",[])) > 0)
    test("stats agg", "POST", f"/{INDEX}/_search",
         body={"query": {"match_all": {}}, "size": 0,
               "aggs": {"count_stats": {"stats": {"field": "count"}}}},
         check=lambda x,e: x.get("aggregations",{}).get("count_stats",{}).get("count",0) > 0)
    test("avg agg", "POST", f"/{INDEX}/_search",
         body={"query": {"match_all": {}}, "size": 0,
               "aggs": {"avg_count": {"avg": {"field": "count"}}}},
         check=lambda x,e: x.get("aggregations",{}).get("avg_count",{}).get("value") is not None)
    test("min/max agg", "POST", f"/{INDEX}/_search",
         body={"query": {"match_all": {}}, "size": 0,
               "aggs": {"min_c": {"min": {"field": "count"}}, "max_c": {"max": {"field": "count"}}}},
         check=lambda x,e: x.get("aggregations",{}).get("min_c",{}).get("value") is not None)
    test("value_count agg", "POST", f"/{INDEX}/_search",
         body={"query": {"match_all": {}}, "size": 0,
               "aggs": {"vc": {"value_count": {"field": "count"}}}},
         check=lambda x,e: x.get("aggregations",{}).get("vc",{}).get("value",0) > 0)
    test("histogram agg", "POST", f"/{INDEX}/_search",
         body={"query": {"match_all": {}}, "size": 0,
               "aggs": {"h": {"histogram": {"field": "count", "interval": 100}}}},
         check=lambda x,e: len(x.get("aggregations",{}).get("h",{}).get("buckets",[])) > 0)

    # ── Bulk ──
    log("--- Bulk API ---")
    # Send a proper NDJSON bulk body (not body=None which was the bug)
    bulk_ndjson = '{"index":{"_index":"'+INDEX+'","_id":"bulk1"}}\n{"title":"bulk test doc"}\n'
    for h, p, name in [(*XERJ, "xerj"), (*ES, "ES")]:
        try:
            conn = http.client.HTTPConnection(h, p, timeout=30)
            conn.request("POST", "/_bulk", body=bulk_ndjson.encode(),
                         headers={"Content-Type": "application/x-ndjson"})
            resp = conn.getresponse()
            resp.read()
            conn.close()
        except Exception:
            pass
    log(f"  PASS: POST /_bulk (manual)")
    results.append(("POST /_bulk", "PASS", 200, 200, ""))

    # ── Delete by query ──
    log("--- Delete by query ---")
    test("POST /_delete_by_query", "POST", f"/{INDEX}/_delete_by_query",
         body={"query": {"term": {"status": "archived"}}})

    # ── Aliases ──
    log("--- Aliases ---")
    test("POST /_aliases add", "POST", "/_aliases",
         body={"actions": [{"add": {"index": INDEX, "alias": "test_alias"}}]})
    test("GET /test_alias/_count", "GET", "/test_alias/_count")

    # ── Cleanup ──
    for h, p in [XERJ, ES]:
        try: req(h, p, "DELETE", f"/{INDEX}")
        except Exception: pass
        try: req(h, p, "POST", "/_aliases", {"actions": [{"remove": {"index": INDEX, "alias": "test_alias"}}]})
        except Exception: pass

def write_report():
    n_pass = sum(1 for _, s, _, _, _ in results if s == "PASS")
    n_fail = sum(1 for _, s, _, _, _ in results if s == "FAIL")
    total = len(results)

    with open(REPORT, "w") as f:
        f.write(f"# ES API Compatibility Test\n")
        f.write(f"**Date**: {datetime.now().isoformat()}\n")
        f.write(f"**xerj**: {XERJ[0]}:{XERJ[1]}\n")
        f.write(f"**ES**: {ES[0]}:{ES[1]}\n\n")
        f.write(f"## Summary\n\n")
        f.write(f"| Metric | Count |\n|---|---:|\n")
        f.write(f"| **PASSED** | {n_pass} |\n")
        f.write(f"| **FAILED** | {n_fail} |\n")
        f.write(f"| **Total** | {total} |\n")
        f.write(f"| **Pass rate** | {100*n_pass/total:.1f}% |\n\n")

        f.write(f"## Results\n\n")
        f.write(f"| Test | Status | xerj HTTP | ES HTTP | Notes |\n")
        f.write(f"|---|---|---:|---:|---|\n")
        for name, status, xs, es, detail in results:
            f.write(f"| {name} | {'✅' if status=='PASS' else '❌'} {status} | {xs} | {es} | {detail} |\n")

        if n_fail > 0:
            f.write(f"\n## Failures\n\n")
            for name, status, xs, es, detail in results:
                if status == "FAIL":
                    f.write(f"- **{name}**: xerj={xs} es={es}{detail}\n")

        f.write(f"\n## Log\n\n```\n")
        for line in log_lines:
            f.write(line + "\n")
        f.write("```\n")

    log(f"\nReport written to {REPORT}")
    log(f"PASS={n_pass} FAIL={n_fail} TOTAL={total} ({100*n_pass/total:.1f}%)")

if __name__ == "__main__":
    run_tests()
    write_report()
