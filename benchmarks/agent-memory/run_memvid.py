"""memvid adapter (memvid/memvid, PyPI memvid-sdk 2.0.160, single-file .mv2 memory).

Configs:
  lex      : SDK defaults, no API key -> find() auto mode resolves to lexical (BM25) search.
  sem_minilm: same file with enable_vec=True; each memory is written with put_many([doc])
             and a local all-MiniLM-L6-v2 embedder (FastEmbed), queries use mode="sem".
Writes are timed per put; commit() runs once in settle (time recorded in meta).
MEMVID_TELEMETRY=0 is set (the SDK sends analytics unless this is 0/false).
Usage: python run_memvid.py lex|sem_minilm
"""
import os, sys, time, tempfile, shutil, json
os.environ["MEMVID_TELEMETRY"] = "0"
cfg = sys.argv[1] if len(sys.argv) > 1 else "lex"
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common
import memvid_sdk
from importlib.metadata import version

d = tempfile.mkdtemp(prefix="memvid_", dir="/dev/shm/memb")
path = os.path.join(d, "bench.mv2")
emb = None
if cfg == "sem_minilm":
    from fastembed import TextEmbedding
    from memvid_sdk.embeddings import EmbeddingProvider
    class MiniLM(EmbeddingProvider):
        def __init__(self):
            self.m = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2", cache_dir="/tmp/fastembed_cache")
        @property
        def dimension(self): return 384
        @property
        def model_name(self): return "all-MiniLM-L6-v2"
        def embed_documents(self, texts): return [list(map(float, v)) for v in self.m.embed(list(texts))]
        def embed_query(self, text): return list(map(float, next(iter(self.m.embed([text])))))
    emb = MiniLM()
mem = memvid_sdk.create(path, enable_vec=(cfg == "sem_minilm"), enable_lex=True)

def add(m):
    if emb is None:
        mem.put(title="memory", label="note", text=m["text"])
    else:
        mem.put_many([{"title": "memory", "label": "note", "text": m["text"]}], embedder=emb)

extra = {}
def settle():
    t = time.perf_counter(); mem.commit(); extra["commit_s"] = round(time.perf_counter() - t, 3)

first = {}
def search(q, k):
    if emb is None:
        r = mem.find(q, k=k)
    else:
        r = mem.find(q, k=k, mode="sem", embedder=emb)
    if not first: first["r"] = r
    return [h.get("snippet") or h.get("text") or "" for h in r.get("hits", [])]

res = common.run("memvid", cfg, add, search, {
    "version": "memvid-sdk " + version("memvid-sdk"), "commit": "PyPI release (repo HEAD e6bd9f7b at test time)",
    "embedder": "all-MiniLM-L6-v2 (FastEmbed, local)" if emb else "none (lexical default)",
    "llm": None, "llm_calls_per_write": 0, "store": "single .mv2 file on tmpfs",
    "notes": "put(title='memory', label='note', text) or put_many([...], embedder); find(q, k)",
}, settle=settle)
res["meta"]["commit_s"] = extra.get("commit_s")
json.dump(res, open(os.path.join(common.HERE, "results", f"memvid__{cfg}.json"), "w"), indent=1)
print(json.dumps(first.get("r"), default=str)[:700])
mem.close(); shutil.rmtree(d, ignore_errors=True)
