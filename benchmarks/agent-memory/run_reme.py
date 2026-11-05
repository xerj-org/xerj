"""ReMe adapter (Alibaba agentscope-ai/ReMe, PyPI reme-ai 0.4.2.1, HTTP service).

Default config: BM25 keyword index, no embeddings, no LLM (the README says
basic file operations and BM25 search run without LLM credentials).
Write = POST /daily_write (one Markdown daily note per memory, date = session).
Read  = POST /search {query, limit}. Index is built by ReMe's file watcher, so
settle() waits until the last written note is searchable and records the wait.
Usage: python3 run_reme.py <path-to-reme-cli>
"""
import json, os, sys, time, shutil, subprocess, urllib.request
import common

REME = sys.argv[1]
CFG = sys.argv[2] if len(sys.argv) > 2 else "bm25_default"
EXTRA = [f"config={os.path.join(common.HERE, 'reme_hybrid_minilm.yaml')}"] if CFG == "hybrid_minilm" else []
PORT = 2444
WS = "/dev/shm/memb/reme_ws"
HOME = "/dev/shm/memb/remehome"
shutil.rmtree(WS, ignore_errors=True); shutil.rmtree(HOME, ignore_errors=True)
os.makedirs(HOME)
env = dict(os.environ, HOME=HOME)
log = open(os.path.join(common.HERE, "tmp", "reme_run.log"), "w")
proc = subprocess.Popen([REME, "start", f"workspace_dir={WS}", f"service.port={PORT}"] + EXTRA, env=env, stdout=log, stderr=log)

def post(path, body):
    req = urllib.request.Request(f"http://127.0.0.1:{PORT}/{path}", data=json.dumps(body).encode(),
                                 headers={"content-type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=120) as r:
        return json.loads(r.read())

for _ in range(120):
    try:
        ver = post("version", {})["answer"]; break
    except Exception:
        time.sleep(0.5)

ds = common.load()
last = ds["memories"][-1]

def add(m):
    r = post("daily_write", {"name": "m-" + m["id"], "description": "memory", "session_id": f"s{m['session']}",
                             "content": m["text"], "date": f"2026-10-0{m['session']}"})
    assert r.get("success"), r

wait = {}
def settle():
    t = time.perf_counter()
    while time.perf_counter() - t < 120:
        r = post("search", {"query": last["text"], "limit": 3})
        if any(x.get("text", "").strip() == last["text"] for x in r["metadata"]["results"]):
            break
        time.sleep(0.5)
    wait["s"] = round(time.perf_counter() - t, 2)

def search(q, k):
    r = post("search", {"query": q, "limit": k})
    return [x.get("text", "") for x in r["metadata"]["results"]]

try:
    res = common.run("reme", CFG, add, search, {
        "version": "reme-ai " + ver, "commit": "PyPI release (repo HEAD 1e51e0dc at test time)",
        "embedder": "all-MiniLM-L6-v2 via local OpenAI-compatible endpoint (emb_server.py)" if EXTRA else "none (default config: embeddings off)", "llm": None, "llm_calls_per_write": 0,
        "store": "Markdown files + BM25 index" + (" + local vector store (hybrid RRF)" if EXTRA else "") + ", workspace on tmpfs",
        "notes": "daily_write per memory; /search limit=k; settle waits for the file watcher",
    }, settle=settle)
    res["meta"]["index_wait_s"] = wait.get("s")
    json.dump(res, open(os.path.join(common.HERE, "results", f"reme__{CFG}.json"), "w"), indent=1)
    print("index wait s", wait.get("s"))
finally:
    proc.terminate(); proc.wait(timeout=30)
