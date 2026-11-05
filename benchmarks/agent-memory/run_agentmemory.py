"""agentmemory adapter (rohitg00/agentmemory, npm @agentmemory/agentmemory 0.9.31).

Server started separately with a temp HOME and fresh data dir:
  HOME=tmp/am/home III_TELEMETRY_ENABLED=false AGENTMEMORY_SECRET=... \
    npx agentmemory --port 3411 --data-dir tmp/am/data
Keyless default: no LLM key, no embedding provider -> BM25 (plus its graph leg).
Write = POST /agentmemory/remember {content}; Read = POST /agentmemory/smart-search {query, limit}.
smart-search returns ids and titles; ids are mapped back to the stored text.
Usage: python3 run_agentmemory.py <config-name>
"""
import json, os, sys, urllib.request
import common

cfg = sys.argv[1] if len(sys.argv) > 1 else "keyless_bm25"
BASE = "http://127.0.0.1:3411/agentmemory/"
H = {"content-type": "application/json", "Authorization": "Bearer " + os.environ.get("AGENTMEMORY_SECRET", "benchsecret123")}

def post(path, body):
    req = urllib.request.Request(BASE + path, data=json.dumps(body).encode(), headers=H, method="POST")
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.loads(r.read())

texts = {}
def add(m):
    r = post("remember", {"content": m["text"], "concepts": []})
    assert r.get("success"), r
    texts[r["memory"]["id"]] = m["text"]

def search(q, k):
    r = post("smart-search", {"query": q, "limit": k})
    return [texts.get(x["obsId"], x.get("title", "")) for x in r.get("results", [])]

ver = json.load(open(os.path.join(common.HERE, "tmp/am/node_modules/@agentmemory/agentmemory/package.json")))["version"]
common.run("agentmemory", cfg, add, search, {
    "version": "@agentmemory/agentmemory " + ver, "commit": "npm release (repo HEAD c6c476d2 at test time)",
    "embedder": os.environ.get("BENCH_EMBEDDER", "none (keyless default, BM25)"), "llm": None, "llm_calls_per_write": 0,
    "store": "iii-engine state worker (local files)",
    "notes": "POST /agentmemory/remember {content}; POST /agentmemory/smart-search {query, limit=k}",
})
