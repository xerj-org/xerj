"""Minimal OpenAI-compatible /v1/embeddings server backed by FastEmbed all-MiniLM-L6-v2 (local).
Gives systems that only accept an OpenAI-style embedding endpoint the same local embedder
as the other systems. Usage: python emb_server.py <port>
"""
import json, sys
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler
from fastembed import TextEmbedding
M = TextEmbedding("sentence-transformers/all-MiniLM-L6-v2", cache_dir="/tmp/fastembed_cache")
N = {"calls": 0}
class H(BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["content-length"])))
        inp = body["input"]; inp = [inp] if isinstance(inp, str) else inp
        N["calls"] += 1
        vecs = [list(map(float, v)) for v in M.embed(inp)]
        out = {"object": "list", "model": body.get("model", "all-MiniLM-L6-v2"),
               "data": [{"object": "embedding", "index": i, "embedding": v} for i, v in enumerate(vecs)],
               "usage": {"prompt_tokens": 0, "total_tokens": 0}}
        b = json.dumps(out).encode()
        self.send_response(200); self.send_header("content-type", "application/json"); self.send_header("content-length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        b = json.dumps(N).encode(); self.send_response(200); self.end_headers(); self.wfile.write(b)
ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
