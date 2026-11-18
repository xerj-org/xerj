#!/usr/bin/env python3
"""Exercise search query compatibility and default budgets through real MCP stdio.

Usage: python3 scripts/mcp-search-contract-smoke.py engine/target/ci-test/xerj
Only an isolated HTTP fixture is contacted; no index or external service is used.
"""
import json
from pathlib import Path
import subprocess
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def main():
    binary = str(Path(sys.argv[1]).resolve(strict=True))
    requests = []
    hits = [{"_index": "docs", "_id": str(i), "_source": {
        "body": "search evidence " * 500, "path": f"source/{i}.txt", "start_line": 1,
    }} for i in range(20)]

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
            requests.append(("GET", self.path, None))
            if self.path == "/":
                self.respond({"version": {"number": "8.13.0"}, "cluster_name": "fixture"})
            elif self.path == "/_cluster/health":
                self.respond({"status": "green", "number_of_nodes": 1})
            elif self.path.startswith("/_cat/indices?"):
                self.respond([{"index": "docs", "docs.count": str(len(hits))}])
            else:
                self.respond({"docs": {"mappings": {"properties": {"body": {"type": "text"}}}}})

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            requests.append(("POST", self.path, body))
            selected = [] if self.path == "/autoindex-catalog/_search" else hits
            self.respond({"hits": {"total": {"value": len(selected)}, "hits": selected}})

    with ThreadingHTTPServer(("127.0.0.1", 0), Handler) as server:
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        url = f"http://127.0.0.1:{server.server_port}"

        def call(args, name="xerj_search"):
            requests.clear()
            messages = [
                {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
                {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                    "name": name, "arguments": args}},
            ]
            run = subprocess.run([binary, "mcp", "--url", url, "--auth", "ApiKey fixture",
                                  "--disable-feedback"], text=True, capture_output=True,
                                 input="".join(json.dumps(m) + "\n" for m in messages),
                                 check=True, timeout=30)
            reply = next(m for m in map(json.loads, run.stdout.splitlines()) if m.get("id") == 2)
            return reply["result"]

        def successful(args):
            result = call(args)
            assert result["isError"] is False, result
            return json.loads(result["content"][0]["text"])

        try:
            clause = {"bool": {"must": {"match": {"body": "search"}}}}
            prior = None
            for field in ("query_dsl", "query"):
                response = successful({"index": "docs", field: clause, "max_tokens": 100000})
                assert requests == [("POST", "/docs/_search", {"query": clause})], requests
                returned = response["hits"]["hits"]
                assert [h["_source"] for h in returned] == [h["_source"] for h in hits], "nonbinding budget must retain full evidence"
                assert returned[0]["match_line"] == 1, "structured queries must retain passage locations"
                if prior is not None:
                    assert returned == prior, "legacy and named DSL must return identical evidence"
                prior = returned
            print("PASS new query_dsl and legacy object query send identical structured requests")

            for args, path in [({"index": "docs"}, "/docs/_search"), ({}, "/ax-*/_search")]:
                successful(args)
                searches = [r for r in requests if r[1] == path]
                assert len(searches) == 1 and searches[0][2]["query"] == {"match_all": {}}, requests
            print("PASS omitted-query browsing and default scope remain available")

            for args in ({"query": "rust", "query_dsl": clause},
                         {"query": clause, "query_dsl": clause},
                         {"query_dsl": "not an object"}, {"query": 42}):
                response = call(args)
                assert response["isError"] is True, response
                assert not requests, "invalid arguments must fail before any node request"
            print("PASS conflicting and mistyped query fields refuse before HTTP")

            for extra in ({}, {"max_tokens": None}):
                response = successful({"index": "docs", "query_dsl": clause, **extra})
                accounting = response["_token_budget"]
                assert accounting["max_tokens"] == 8000 and accounting["tokens_used"] <= 8000, accounting
            print("PASS absent/null budget applies 8000 words with explicit trimming accounting")

            result = call({}, name="xerj_status")
            assert result["isError"] is False, result
            text = result["content"][0]["text"]
            assert "docs  20 docs" in text and "autoindex catalog: ABSENT" in text, text
            assert "nothing indexed" not in text, text
            assert len(requests) == 3 and all(r[0] == "GET" for r in requests), requests
            print("PASS status tool dispatch reports ordinary indexed data without a catalog")
        finally:
            server.shutdown()
            thread.join(timeout=5)


if __name__ == "__main__":
    main()
