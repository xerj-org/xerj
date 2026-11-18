"""mcp-memory-service adapter (doobidoo/mcp-memory-service, PyPI 12.4.0 with the [sqlite] extra).

Uses the same service layer as its MCP tools: MemoryService(SqliteVecMemoryStorage).
store_memory(content) / retrieve_memories(query, n_results=k).
Defaults kept: sqlite-vec backend, ONNX all-MiniLM-L6-v2 embedder, semantic dedup on.
Usage: python run_mms.py default
"""
import os, sys, tempfile, shutil, json, asyncio
cfg = sys.argv[1] if len(sys.argv) > 1 else "default"
d = tempfile.mkdtemp(prefix="mms_", dir="/dev/shm/memb")
os.environ["HOME"] = d
os.environ["MCP_MEMORY_BASE_DIR"] = d
os.environ["MCP_MEMORY_STORAGE_BACKEND"] = "sqlite_vec"
os.environ["MCP_MEMORY_SQLITE_PATH"] = os.path.join(d, "mem.db")
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import common
from importlib.metadata import version
from mcp_memory_service.storage.sqlite_vec import SqliteVecMemoryStorage
from mcp_memory_service.services.memory_service import MemoryService

loop = asyncio.new_event_loop()
st = SqliteVecMemoryStorage(os.path.join(d, "mem.db"))
loop.run_until_complete(st.initialize())
svc = MemoryService(st)
rejected = []
first = {}

def add(m):
    r = loop.run_until_complete(svc.store_memory(content=m["text"]))
    if not r.get("success", False):
        rejected.append({"id": m["id"], "resp": json.dumps(r, default=str)[:300]})

def search(q, k):
    r = loop.run_until_complete(svc.retrieve_memories(query=q, n_results=k))
    if not first: first["r"] = r
    return [x.get("content", "") for x in r.get("memories", [])]

res = common.run("mcp-memory-service", cfg, add, search, {
    "version": "mcp-memory-service " + version("mcp-memory-service"), "commit": "PyPI release (repo HEAD 0071ab2a at test time)",
    "embedder": "all-MiniLM-L6-v2 (default, ONNX)", "llm": None, "llm_calls_per_write": 0,
    "store": "sqlite-vec file on tmpfs", "notes": "MemoryService.store_memory / retrieve_memories, defaults (semantic dedup on)",
})
res["meta"]["rejected_writes"] = rejected
json.dump(res, open(os.path.join(common.HERE, "results", f"mcp-memory-service__{cfg}.json"), "w"), indent=1)
print("rejected", len(rejected), rejected[:3])
print(json.dumps(first.get("r"), default=str)[:600])
shutil.rmtree(d, ignore_errors=True)
