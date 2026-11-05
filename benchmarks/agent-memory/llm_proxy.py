"""Counting proxy in front of the local llama.cpp server. Stdlib only.
Every POST is forwarded unchanged; the count of chat/completion calls is kept
in llm_calls.json so adapters can report LLM calls per write.
Usage: python3 llm_proxy.py <listen_port> <upstream_base>
"""
import json, sys, threading, urllib.request
from http.server import ThreadingHTTPServer, BaseHTTPRequestHandler

PORT, UP = int(sys.argv[1]), sys.argv[2].rstrip("/")
lock = threading.Lock()
counts = {"chat": 0, "embeddings": 0, "other": 0}

class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass
    def _fwd(self, method):
        n = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(n) if n else None
        if self.path.startswith("/_count"):
            data = json.dumps(counts).encode()
            if method == "POST" and self.path.endswith("reset"):
                with lock:
                    for k in counts: counts[k] = 0
            self.send_response(200); self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(data))); self.end_headers(); self.wfile.write(data); return
        kind = "chat" if "completions" in self.path else ("embeddings" if "embed" in self.path else "other")
        if method == "POST":
            with lock: counts[kind] += 1
        path = self.path
        req = urllib.request.Request(UP + path, data=body, method=method,
                                     headers={"content-type": self.headers.get("content-type", "application/json")})
        try:
            with urllib.request.urlopen(req, timeout=900) as r:
                data = r.read(); code = r.status; ct = r.headers.get("content-type", "application/json")
        except urllib.error.HTTPError as e:
            data = e.read(); code = e.code; ct = "application/json"
        self.send_response(code); self.send_header("content-type", ct)
        self.send_header("content-length", str(len(data))); self.end_headers(); self.wfile.write(data)
    def do_POST(self): self._fwd("POST")
    def do_GET(self): self._fwd("GET")

ThreadingHTTPServer(("127.0.0.1", PORT), H).serve_forever()
