"""mem0 adapter (mem0ai 2.2.1, open-source Memory class, fully local).

Embedder: FastEmbed sentence-transformers/all-MiniLM-L6-v2 (ONNX, 384 dims).
Vector store: Qdrant local mode (mem0's default store, keyword search on).
LLM: local Qwen2.5-3B-Instruct Q4_K_M through llama.cpp's OpenAI API, behind
llm_proxy.py on :8098 so calls are counted. Telemetry off (MEM0_TELEMETRY=False).
Usage: python run_mem0.py raw|infer
  raw   = add(..., infer=False): stores the text as is, no LLM call
  infer = add(..., infer=True): mem0's LLM fact extraction and update step
"""
import os, sys, json, tempfile, shutil, urllib.request
os.environ["MEM0_TELEMETRY"] = "False"
cfg = sys.argv[1]
d = tempfile.mkdtemp(prefix="mem0bench_", dir=os.path.join(os.path.dirname(__file__), "tmp"))
os.environ["MEM0_DIR"] = d
import common
from mem0 import Memory
import mem0 as m0pkg
from importlib.metadata import version

config = {
    "embedder": {"provider": "fastembed", "config": {"model": "sentence-transformers/all-MiniLM-L6-v2", "embedding_dims": 384}},
    "vector_store": {"provider": "qdrant", "config": {"path": os.path.join(d, "qdrant"), "embedding_model_dims": 384, "collection_name": "bench", "on_disk": True}},
    "llm": {"provider": "openai", "config": {"model": "qwen2.5-3b-instruct-q4_k_m", "openai_base_url": "http://127.0.0.1:8098/v1", "api_key": "local", "temperature": 0.0, "max_tokens": 1500}},
    "history_db_path": os.path.join(d, "history.db"),
}
m = Memory.from_config(config)

def count():
    return json.loads(urllib.request.urlopen("http://127.0.0.1:8098/_count").read())["chat"]

c0 = count()
def add(mem):
    m.add(mem["text"], user_id="dana", infer=(cfg == "infer"), metadata={"session": mem["session"]})

def search(q, k):
    r = m.search(q, top_k=k, filters={"user_id": "dana"})
    return [x["memory"] for x in r.get("results", [])]

calls = {}
def settle():
    calls["n"] = count() - c0

import collections
res = common.run("mem0", cfg, add, search, {
    "version": "mem0ai " + version("mem0ai"), "commit": "PyPI release (source HEAD 753496cf at test time)",
    "embedder": "sentence-transformers/all-MiniLM-L6-v2 via FastEmbed (ONNX)",
    "llm": "Qwen2.5-3B-Instruct Q4_K_M, llama.cpp b12019" if cfg == "infer" else None,
    "vector_store": "Qdrant local mode",
    "notes": f"add(infer={cfg=='infer'}), search(top_k=k, default threshold 0.1)",
}, settle=settle)
n = calls.get("n", 0)
res["meta"]["llm_calls_total_ingest"] = n
res["meta"]["llm_calls_per_write"] = round(n / res["n_memories"], 2)
res["meta"]["llm_calls_total_including_queries"] = count() - c0
p = os.path.join(common.HERE, "results", f"mem0__{cfg}.json")
json.dump(res, open(p, "w"), indent=1)
print("llm calls ingest", n)
shutil.rmtree(d, ignore_errors=True)
