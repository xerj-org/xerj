#!/usr/bin/env python3
"""Compare the real CLI and MCP over a deterministic HTTP fixture (#2080).

Usage: python3 scripts/mcp-def-smoke.py engine/target/ci-test/xerj
No real node, external corpus or model is needed. Engine behavior is covered
separately by `cargo test -p xerj-mcp --test local_def`.
"""
import json
import subprocess
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def main():
    binary = sys.argv[1]
    requests = []
    catalog_reads = []
    mapping = {"project-code": {"mappings": {"properties": {
        "name": {"type": "keyword"}, "kind": {"type": "keyword"},
        "language": {"type": "keyword"}, "code": {"type": "text"},
        "impl": {"type": "text"}, "ax_path": {"type": "keyword"},
        "line": {"type": "integer"},
    }}}}
    hits = [{"_id": name, "_score": score, "_source": {
        "ax_path": path, "line": 7, "kind": kind, "code": code,
    }} for name, score, path, kind, code in [
        ("alias", 12, "src/alias.rs", "type", "type Cache = Store;"),
        ("test", 11, "tests/cache.rs", "struct", "struct Cache"),
        ("real", 10, "src/cache.rs", "struct", "pub struct Cache"),
        ("duplicate", 9, "src/cache.rs", "struct", "pub struct Cache"),
    ]]

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def respond(self, body):
            data = json.dumps(body).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self):
            self.respond(mapping if self.path.endswith("/_mapping") else {})

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            if self.path == "/autoindex-catalog/_search":
                # Staleness-hint catalog read (#2426), made by both CLI and MCP.
                catalog_reads.append(self.path)
            else:
                requests.append((self.path, body))
            self.respond({"hits": {"total": {"value": len(hits)}, "hits": hits}})

    with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        url = f"http://127.0.0.1:{server.server_port}"

        def run(args, stdin=None):
            return subprocess.run([binary, *args], input=stdin, text=True,
                                  capture_output=True, timeout=30, check=True).stdout

        try:
            for symbol, filters in [("Cache", {}), ("cache entries", {}),
                                    ("Cache", {"kind": "struct", "lang": "rust"})]:
                requests.clear()
                catalog_reads.clear()
                cli_args = ["def", "--url", url, "--api-key", "fixture",
                            "--prefix", "project", "-k", "2", "--json",
                            "--disable-feedback", symbol]
                for key, value in filters.items():
                    cli_args += [f"--{key}", value]
                cli = json.loads(run(cli_args))
                messages = [
                    {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
                    {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                        "name": "xerj_def", "arguments": {
                            "symbol": symbol, "prefix": "project", "k": 2, **filters}}},
                ]
                stdout = run(["mcp", "--url", url, "--auth", "ApiKey fixture",
                              "--disable-feedback"],
                             "".join(json.dumps(m) + "\n" for m in messages))
                reply = next(m for m in map(json.loads, stdout.splitlines()) if m.get("id") == 2)
                assert reply["result"]["isError"] is False, reply
                mcp = json.loads(reply["result"]["content"][0]["text"])
                assert len(requests) == 2 and requests[0] == requests[1], requests
                assert len(catalog_reads) == 2, catalog_reads
                assert requests[0][0] == "/project-*/_search", requests
                assert cli == mcp, (cli, mcp)
                assert [h["_id"] for h in mcp["hits"]["hits"]] == ["real", "alias"], mcp
                print(f"PASS CLI/MCP request and ranked response parity: {symbol!r} {filters}")
        finally:
            server.shutdown()
            thread.join(timeout=5)


if __name__ == "__main__":
    main()
