#!/usr/bin/env python3
"""Regression tests: an HTTP failure is never scored or timed (#2075 items 11, 13).

Before the fix `common.http` turned a non-2xx answer into {"_err": ...}; eval.py
then read it as a query with no hits (counted in `empty=`, nDCG 0) and
latency.py appended its round-trip to the latency samples. Both scripts run
end to end against an in-process stub node. Stdlib only, no engine needed.

    python3 benchmarks/neural-path-triage/test_http_failures.py
"""
import json
import pathlib
import subprocess
import sys
import tempfile
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = pathlib.Path(__file__).resolve().parent

QUERIES = {"q1": "alpha", "q2": "boom", "q3": "nothing"}
QRELS = {"q1": "d1", "q2": "d2", "q3": "d3"}


class Stub(BaseHTTPRequestHandler):
    """`_search` fails with 500 when the body mentions a word in `fail_on`;
    a query for "nothing" is a genuine zero-hit answer; anything else hits d1."""

    fail_on = ()

    def log_message(self, *_):
        pass

    def reply(self, status, body):
        raw = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        self.reply(200, {"count": 3})

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length") or 0)).decode()
        if any(word in body for word in self.fail_on):
            return self.reply(500, {"error": "stub failure"})
        hits = [] if "nothing" in body else [{"_id": "d1", "_score": 1.0}]
        self.reply(200, {"took": 2, "hits": {"hits": hits}})


class Harness(unittest.TestCase):
    def setUp(self):
        Stub.fail_on = ()
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Stub)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.url = f"http://127.0.0.1:{self.server.server_address[1]}"
        self.tmp = tempfile.TemporaryDirectory()
        data = pathlib.Path(self.tmp.name)
        (data / "qrels").mkdir()
        (data / "queries.jsonl").write_text(
            "".join(json.dumps({"_id": k, "text": v}) + "\n" for k, v in QUERIES.items()))
        (data / "qrels" / "test.tsv").write_text(
            "query-id\tcorpus-id\tscore\n" + "".join(f"{q}\t{d}\t1\n" for q, d in QRELS.items()))

    def tearDown(self):
        self.server.shutdown()
        self.server.server_close()
        self.tmp.cleanup()

    def drop_query(self, qid):
        data = pathlib.Path(self.tmp.name)
        (data / "qrels" / "test.tsv").write_text(
            "query-id\tcorpus-id\tscore\n"
            + "".join(f"{q}\t{d}\t1\n" for q, d in QRELS.items() if q != qid))

    def run_script(self, *args):
        return subprocess.run([sys.executable, str(HERE / args[0]), self.url, "idx", self.tmp.name,
                               *args[1:]], capture_output=True, text=True, timeout=60, cwd=HERE)


class EvalSeparatesFailuresFromEmpty(Harness):
    def test_http_failure_is_not_scored_as_empty(self):
        Stub.fail_on = ("boom",)
        r = self.run_script("eval.py")
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertIn("HTTP 500", r.stderr)
        self.assertNotIn("nDCG@10=", r.stdout)

    def test_genuine_empty_result_is_still_counted_and_scored(self):
        self.drop_query("q2")
        r = self.run_script("eval.py")
        self.assertEqual(r.returncode, 0, r.stderr)
        bm25 = next(line for line in r.stdout.splitlines() if line.startswith("bm25 "))
        self.assertIn("nDCG@10=0.5000", bm25)
        self.assertIn("empty=  1", bm25)


class LatencyRejectsFailedRequests(Harness):
    def test_failed_request_is_not_a_latency_sample(self):
        Stub.fail_on = ("boom",)
        r = self.run_script("latency.py", "3", "0")
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertIn("HTTP 500", r.stderr)
        self.assertNotIn("p50=", r.stdout)

    def test_successful_requests_are_timed(self):
        r = self.run_script("latency.py", "1", "0")
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(r.stdout.count("server took p50="), 5)


if __name__ == "__main__":
    unittest.main()
