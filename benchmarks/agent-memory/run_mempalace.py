"""MemPalace adapter (mempalace 3.11.0 from PyPI, in-process MCP tool functions).

Write = mempalace_add_drawer (verbatim text into one wing/room).
Read  = mempalace_search (default candidate_strategy="vector").
Store: ChromaDB with MemPalace's default embedder (all-MiniLM-L6-v2, ONNX).
No LLM. Chroma anonymized telemetry is off by MemPalace's own setting.
Usage: python run_mempalace.py default
"""
import os, sys, tempfile, shutil, json
cfg = sys.argv[1] if len(sys.argv) > 1 else "default"
d = tempfile.mkdtemp(prefix="mpbench_", dir="/dev/shm/memb")
os.environ["MEMPALACE_PALACE_PATH"] = os.path.join(d, "palace")
os.environ["MEMPALACE_CONFIG_DIR"] = os.path.join(d, "cfg")
os.environ["HOME"] = d  # keep any dotfiles out of the real home
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common
from mempalace.mcp_server import tool_add_drawer, tool_search
from importlib.metadata import version

def add(m):
    r = tool_add_drawer(wing="dana", room="notes", content=m["text"], added_by="bench")
    if not r.get("success", True) and "duplicate" not in json.dumps(r).lower():
        raise RuntimeError(r)

first = {}
def search(q, k):
    r = tool_search(q, limit=k)
    if not first:
        first["r"] = r
    hits = r.get("results") or r.get("hits") or []
    return [h.get("text") or h.get("content") or h.get("document") for h in hits]

common.run("mempalace", cfg, add, search, {
    "version": "mempalace " + version("mempalace"), "commit": "PyPI release",
    "embedder": "all-MiniLM-L6-v2 (MemPalace default, ONNX via Chroma)",
    "llm": None, "llm_calls_per_write": 0, "vector_store": "ChromaDB (embedded)",
    "notes": "tool_add_drawer(wing='dana', room='notes'); tool_search(query, limit=k)",
})
print(json.dumps(first.get("r"), default=str)[:600])
shutil.rmtree(d, ignore_errors=True)
