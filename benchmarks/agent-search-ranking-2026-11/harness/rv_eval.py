"""#1788 exact-ID lane on the pinned rust-vulns pack (2026-10-24, sha256 54f0419e...).

Re-uses the pinned workload (ccc159ffe tools/benchmarks/batch-retrieval/cases.json)
and its scoring rule, on a private node in this lane's port range (the pinned
run.py binds random ports, which this lane may not use). Two workloads per client:

  pinned : one xerj_batch_search call, 50 items, size 3 (same args as run.py)
  idq    : the 30 canonical + alias IDs sent as plain `query` text, bare and
           inside a sentence, through xerj_batch_search (the shared
           search_query_body_for_mapping path). Success = labelled ID at rank 1.

usage: rv_eval.py <url> <pack.zip> <cases.json> <binary> <label> [--ingest] [--repeats N]
"""
import hashlib, io, json, os, statistics, subprocess, sys, time, urllib.request, zipfile

URL, PACK, CASES, BIN, LABEL = sys.argv[1:6]
INGEST = "--ingest" in sys.argv
REPEATS = int(sys.argv[sys.argv.index("--repeats") + 1]) if "--repeats" in sys.argv else 3
INDEX = "advisories"
SHA = "54f0419e6d944635eeaf9199923064278c36fd578213db60ca77cc85d2a69e3d"


def request(method, path, value=None, ndjson=False):
    data = (value.encode() if ndjson else json.dumps(value).encode()) if value is not None else None
    req = urllib.request.Request(URL + path, data, method=method, headers={
        "Content-Type": "application/x-ndjson" if ndjson else "application/json"})
    with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(req, timeout=120) as r:
        return json.load(r)


data = open(PACK, "rb").read()
assert hashlib.sha256(data).hexdigest() == SHA, "pack sha mismatch"
records = []
with zipfile.ZipFile(io.BytesIO(data)) as z:
    manifest = json.loads(z.read("manifest.json"))
    for name, entry in sorted(manifest["files"].items()):
        blob = z.read(name)
        assert hashlib.sha256(blob).hexdigest() == entry["sha256"], name
        if name.startswith("records-") and name.endswith(".jsonl"):
            records.extend(json.loads(l) for l in blob.splitlines())
assert len(records) == manifest["counts"]["records"]
cases = json.load(open(CASES))

if INGEST:
    request("PUT", "/" + INDEX, {"mappings": {"dynamic": False, "properties": {
        "id": {"type": "keyword"}, "aliases": {"type": "keyword"},
        "body": {"type": "text"}, "title": {"type": "text"},
        "ax_path": {"type": "keyword"}, "ax_locator": {"type": "keyword"}}}})
    for s in range(0, len(records), 100):
        lines = []
        for r in records[s:s + 100]:
            lines.append(json.dumps({"index": {"_index": INDEX, "_id": r["id"]}}) + "\n")
            lines.append(json.dumps(dict(r, ax_path="rust-vulns/records", ax_locator=r["id"]),
                                    ensure_ascii=False) + "\n")
        res = request("POST", "/_bulk", "".join(lines), ndjson=True)
        assert res.get("errors") is False, str(res)[:300]
    request("POST", "/" + INDEX + "/_refresh")
assert request("GET", "/" + INDEX + "/_count")["count"] == len(records)

env = dict(os.environ, XERJ_DISABLE_FEEDBACK="true")
p = subprocess.Popen([BIN, "mcp", "--url", URL], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                     stderr=subprocess.DEVNULL, text=True, env=env)
seq = [0]


def rpc(method, params):
    seq[0] += 1
    p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": seq[0], "method": method, "params": params}) + "\n")
    p.stdin.flush()
    while True:
        msg = json.loads(p.stdout.readline())
        if msg.get("id") == seq[0]:
            return msg["result"]


rpc("initialize", {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "rv", "version": "0"}})
p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
p.stdin.flush()


def batch(queries, size=3):
    t0 = time.perf_counter()
    res = rpc("tools/call", {"name": "xerj_batch_search",
                             "arguments": {"index": INDEX, "size": size, "queries": queries}})
    ms = (time.perf_counter() - t0) * 1000
    text = res["content"][0]["text"]
    try:
        return json.loads(text), ms
    except json.JSONDecodeError:
        raise SystemExit("non-JSON tool result: " + text[:600])


def ids_of(row):
    return [e["citation"]["_id"] for e in row.get("hits", [])] if row.get("status") == "hits" else []


def score(case, row):
    exp = set(case["expected_ids"])
    st = row.get("status")
    if case["kind"] == "invalid":
        return st == "error"
    if not exp:
        return st == "abstain"
    if case["kind"] == "text":
        return st == "hits" and bool(exp & set(ids_of(row)))
    return st == "hits" and set(ids_of(row)) == exp


id_cases = [c for c in cases if c["kind"] in ("canonical", "alias")]
idq = []
for c in id_cases:
    ident = c["input"]["identifier"]
    idq.append(({"key": "bare-" + c["input"]["key"], "query": ident}, c["expected_ids"]))
    idq.append(({"key": "sent-" + c["input"]["key"], "query": f"what does advisory {ident} affect"},
                c["expected_ids"]))

out = {"label": LABEL, "binary_sha256": hashlib.sha256(open(BIN, "rb").read()).hexdigest(),
       "pack_sha256": SHA, "records": len(records), "repeats": REPEATS, "trials": []}
for rep in range(REPEATS):
    load = open("/proc/loadavg").read().split()[:3]
    resp, ms_pinned = batch([c["input"] for c in cases])
    rows = resp["results"]
    by_kind = {}
    for c, r in zip(cases, rows):
        k = c["kind"]
        ok = score(c, r)
        by_kind.setdefault(k, [0, 0])
        by_kind[k][0] += int(ok)
        by_kind[k][1] += 1
    half = len(idq) // 2
    ra, ma = batch([q for q, _ in idq[:half]])
    rb, mb = batch([q for q, _ in idq[half:]])
    resp2, ms_idq = {"results": ra["results"] + rb["results"]}, ma + mb
    hit1 = {"bare": [0, 0], "sent": [0, 0]}
    misses = []
    for (q, exp), r in zip(idq, resp2["results"]):
        kind = q["key"].split("-", 1)[0]
        got = ids_of(r)
        ok = bool(got) and got[0] in exp
        hit1[kind][0] += int(ok)
        hit1[kind][1] += 1
        if not ok:
            misses.append({"query": q["query"], "expected": exp, "top3": got})
    out["trials"].append({"load": load, "pinned_ms": round(ms_pinned, 1), "pinned_by_kind": by_kind,
                          "idq_ms": round(ms_idq, 1), "idq_hit1": hit1, "idq_misses": misses})
p.stdin.close()
p.wait(timeout=30)
t = out["trials"]
out["summary"] = {
    "pinned_correct": f"{sum(v[0] for v in t[0]['pinned_by_kind'].values())}/50",
    "pinned_by_kind": t[0]["pinned_by_kind"],
    "idq_bare_hit1": "{}/{}".format(*t[0]["idq_hit1"]["bare"]),
    "idq_sentence_hit1": "{}/{}".format(*t[0]["idq_hit1"]["sent"]),
    "pinned_ms_median": statistics.median(x["pinned_ms"] for x in t),
    "idq_ms_median": statistics.median(x["idq_ms"] for x in t),
    "stable_across_repeats": all(x["pinned_by_kind"] == t[0]["pinned_by_kind"]
                                 and x["idq_hit1"] == t[0]["idq_hit1"] for x in t),
}
print(json.dumps(out, indent=1, ensure_ascii=False))
