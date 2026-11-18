#!/usr/bin/env python3
"""A loopback stand-in for hub.xerj.org, used by whole_hub_e2e.sh.

    static_hub.py --root <dir> --port <port>

Serves the files under <dir> read-only (no directory listings; anything else is a
404), with one exception that copies what the real static site does today: every
path under /html/ answers 200 with the site's HTML home page, so
<base>/html/api/v1/catalogue.json is "a static host's HTML page with status 200"
(spec §12: the client must read that as "unavailable", never as data).

Each request is logged to stderr as `GET <path> <status>`; the gate reads that log
to prove an --offline run made no request at all.
"""

import argparse
import http.server
import pathlib
import sys

HOME_PAGE = b"""<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>XERJ Corpus Hub</title></head>
<body><h1>XERJ Corpus Hub</h1><p>The static site's home page.</p></body></html>
"""


def handler_for(root: pathlib.Path):
    class Handler(http.server.BaseHTTPRequestHandler):
        def _send(self, status: int, body: bytes, ctype: str) -> None:
            self.send_response(status)
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(body)

        def do_GET(self):  # noqa: N802 (http.server's naming)
            path = self.path.split("?", 1)[0]
            if path.startswith("/html/") or path == "/html":
                self._send(200, HOME_PAGE, "text/html; charset=utf-8")
                return
            target = (root / path.lstrip("/")).resolve()
            if root not in target.parents or not target.is_file():
                self._send(404, b"not found\n", "text/plain")
                return
            ctype = "application/json" if target.suffix == ".json" else "application/octet-stream"
            self._send(200, target.read_bytes(), ctype)

        do_HEAD = do_GET

        def log_message(self, fmt, *args):
            pass

        def log_request(self, code="-", size="-"):
            print(f"{self.command} {self.path} {code}", file=sys.stderr, flush=True)

    return Handler


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", required=True)
    ap.add_argument("--port", type=int, required=True)
    a = ap.parse_args()
    root = pathlib.Path(a.root).resolve()
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", a.port), handler_for(root))
    srv.serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
