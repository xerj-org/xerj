#!/usr/bin/env python3
"""A stand-in for the xerj binary, for the scripts/bench guard tests.

Invoked exactly as lib/repro.sh invokes the real binary
(`--insecure --port P --data-dir D`) and answers just enough of the wire API
for knn-recall.sh and query-latency-size0.sh to reach their measurement step.
Two knobs pick the failure a test wants the script to notice:

  FAKE_SEARCH_STATUS  HTTP status every `_search` answers with (default 200)
  FAKE_ANN            `ready` (default, every vector graphed), `stale`
                      (half graphed, stale=true), `partial` (1 of 50
                      graphed, not stale) or `empty` (no row for the index)

Stdlib only. Never binds anything but 127.0.0.1.
"""
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def arg(name, default=None):
    argv = sys.argv[1:]
    return argv[argv.index(name) + 1] if name in argv else default


if "--version" in sys.argv[1:]:
    print("xerj fake-for-tests 0.0.0")
    raise SystemExit(0)

SEARCH_STATUS = int(os.environ.get("FAKE_SEARCH_STATUS", "200"))
ANN = os.environ.get("FAKE_ANN", "ready")


def ann_row(graphed, vector_docs, covered, stale):
    """The shape GET /_cat/ann?format=json returns: `coverage` is a doc COUNT."""
    return {"index": "bench-vec", "present": True, "nodes": graphed, "vector_docs": vector_docs,
            "coverage": graphed, "covered": covered, "stale": stale, "rebuilding": False}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def send(self, status, body):
        raw = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def drain(self):
        n = int(self.headers.get("content-length") or 0)
        return self.rfile.read(n) if n else b""

    def route(self):
        self.drain()
        path = self.path.split("?", 1)[0]
        if path.startswith("/_cat/ann"):
            rows = {
                "ready": [ann_row(50, 50, True, False)],
                "stale": [ann_row(25, 50, False, True)],
                "partial": [ann_row(1, 50, False, False)],
                "empty": [],
            }[ANN]
            return self.send(200, rows)
        if path.endswith("/_search"):
            if SEARCH_STATUS >= 300:
                return self.send(SEARCH_STATUS, {"error": "fake failure", "status": SEARCH_STATUS})
            hits = [{"_id": str(i), "_score": 0.5} for i in range(10)]
            return self.send(200, {"took": 1, "hits": {"total": {"value": 10}, "hits": hits}})
        if path.endswith("/_bulk"):
            return self.send(200, {"errors": False, "items": []})
        if path.endswith("/_count"):
            return self.send(200, {"count": 0})
        return self.send(200, {"acknowledged": True})

    do_GET = do_POST = do_PUT = do_DELETE = route

    def do_HEAD(self):
        self.send_response(200)
        self.send_header("content-length", "0")
        self.end_headers()


ThreadingHTTPServer(("127.0.0.1", int(arg("--port"))), Handler).serve_forever()
