"""XERJ adapter. Talks to a running node over HTTP (stdlib only).

Start the node first (see RUN.md):
  XERJ_DISABLE_FEEDBACK=true xerj --insecure --port 9411 -d <tmpdir> --embed-mode neural
Usage: python3 run_xerj.py <config> where config is bm25 | semantic | hybrid | hybrid_recency
"""
import json, os, sys, urllib.request, subprocess, uuid
import common

BASE = "http://127.0.0.1:9411"
cfg = sys.argv[1]
ns = f"bench-{cfg}-{uuid.uuid4().hex[:6]}"

def post(path, body):
    req = urllib.request.Request(BASE + path, data=json.dumps(body).encode(),
                                 headers={"content-type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.loads(r.read())

def add(m):
    post(f"/_memory/{ns}", {"text": m["text"], "metadata": {"session": m["session"]}})

def settle():
    try:
        post(f"/{'.xerj-memory-' + ns}/_refresh", {})
    except Exception:
        pass

def search(q, k):
    body = {"query": q, "k": k}
    if cfg == "semantic":
        body["semantic"] = True
    elif cfg == "hybrid":
        body["hybrid"] = True
    elif cfg == "hybrid_recency":
        body["hybrid"] = True
        body["recency_weight"] = 0.3
    r = post(f"/_memory/{ns}/_recall", body)
    return [h.get("text") or h.get("_source", {}).get("text") for h in r.get("hits", [])]

ver = subprocess.run([sys.argv[2] if len(sys.argv) > 2 else os.environ.get("XERJ_BIN", "xerj"), "--version"], capture_output=True, text=True).stdout.strip()
common.run("xerj", cfg, add, search, {
    "version": ver, "commit": "release tag v1.0.0-rc.100",
    "embedder": "none (BM25)" if cfg == "bm25" else "all-MiniLM-L6-v2 (built-in, --embed-mode neural)",
    "llm": None, "llm_calls_per_write": 0,
    "notes": "one node, --insecure, loopback, namespace per config; recall body: " + cfg,
}, settle=settle)
