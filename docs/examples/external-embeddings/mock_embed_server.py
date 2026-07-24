#!/usr/bin/env python3
"""A tiny OpenAI-compatible /v1/embeddings server, to prove XERJ's external
embeddings integration end to end without needing a real provider key.
Deterministic, dependency-free. Logs every call so we can show XERJ hit it."""
import json, hashlib, http.server, sys

DIM = 256
CALLS = {"n": 0, "texts": 0}

def embed(text: str):
    # Deterministic bag-of-trigram hashed vector, L2-normalised — enough for
    # meaningful nearest-neighbour behaviour in the verification.
    v = [0.0] * DIM
    t = text.lower()
    for i in range(len(t) - 2):
        h = int(hashlib.md5(t[i:i+3].encode()).hexdigest(), 16)
        v[h % DIM] += 1.0
    n = sum(x * x for x in v) ** 0.5 or 1.0
    return [x / n for x in v]

class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        texts = body.get("input", [])
        CALLS["n"] += 1; CALLS["texts"] += len(texts)
        sys.stderr.write(f"[mock-embeddings] call #{CALLS['n']}: {len(texts)} text(s), model={body.get('model')}\n"); sys.stderr.flush()
        out = {"object": "list", "model": body.get("model", "mock"),
               "data": [{"object": "embedding", "index": i, "embedding": embed(t)} for i, t in enumerate(texts)],
               "usage": {"prompt_tokens": CALLS["texts"], "total_tokens": CALLS["texts"]}}
        b = json.dumps(out).encode()
        self.send_response(200); self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)
    def do_GET(self):
        b = json.dumps({"calls": CALLS["n"], "texts": CALLS["texts"]}).encode()
        self.send_response(200); self.send_header("Content-Length", str(len(b))); self.end_headers(); self.wfile.write(b)

if __name__ == "__main__":
    http.server.HTTPServer(("127.0.0.1", 8900), H).serve_forever()
