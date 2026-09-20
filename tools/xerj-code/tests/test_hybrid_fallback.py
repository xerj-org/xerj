#!/usr/bin/env python3
"""Offline regressions for optional transport failures in hybrid retrieval."""
import contextlib
import http.client
import importlib.util
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import urllib.error


XC_PATH = Path(__file__).resolve().parents[1] / "scripts" / "xc.py"
PREFIX = "xc-fixture"
INDEX = PREFIX + "-1"


def hit(name, score):
    return {"_index": INDEX, "_id": name, "_score": score,
            "_source": {"path": f"fixture/{name}.rs",
                        "body": f"fn parse_{name}() {{}}"}}


BM25_HITS = [hit("alpha", 9.0), hit("beta", 7.0), hit("gamma", 5.0)]
VECTOR_HITS = [hit("beta", 0.9), hit("delta", 0.8)]

# A connection can fail while opening a request or while reading its body.
# These are concrete urllib/http.client failures, without network or timing.
FAILURES = (
    ("url", lambda: urllib.error.URLError("fixture connection refused"), False),
    ("timeout", lambda: TimeoutError("fixture read timed out"), True),
    ("reset", lambda: ConnectionResetError("fixture connection reset"), True),
    ("disconnect", lambda: http.client.RemoteDisconnected("fixture disconnect"), False),
    ("incomplete", lambda: http.client.IncompleteRead(b'{"hits":', 10), True),
)


class BrokenResponse(io.BytesIO):
    def __init__(self, failure):
        super().__init__()
        self.failure = failure

    def read(self, *args):
        raise self.failure()


class Transport:
    """Serve real request shapes; optionally fail one retrieval stage."""

    def __init__(self, stage=None, failure=None, during_read=False, bm25=None,
                 semantic_only=False, vector=None, capable=True):
        self.stage = stage
        self.failure = failure
        self.during_read = during_read
        self.bm25 = BM25_HITS if bm25 is None else bm25
        self.vector = VECTOR_HITS if vector is None else vector
        self.semantic_only = semantic_only
        self.capable = capable
        self.calls = []
        self.mapping_calls = 0

    def __call__(self, req, timeout=None):
        url = req.full_url
        if "/_cat/indices/" in url:
            stage, payload = "loaded", [{"index": INDEX}]
        elif url.endswith("/_mapping"):
            self.mapping_calls += 1
            stage = "fields" if self.mapping_calls == 1 and not self.semantic_only else "mapping"
            payload = {INDEX: {"mappings": {"properties": {
                "body": {"type": "semantic_text" if self.capable else "text"},
                "defs": {"type": "text"}, "title": {"type": "text"},
            }}}}
        elif url.endswith("/_search"):
            body = json.loads(req.data)
            stage = "vector" if "semantic" in body["query"] else "bm25"
            payload = {"hits": {"hits": self.vector if stage == "vector" else self.bm25}}
        else:
            raise AssertionError(f"unexpected request: {url}")
        self.calls.append(stage)
        if stage == self.stage:
            if self.during_read:
                return BrokenResponse(self.failure)
            raise self.failure()
        return io.BytesIO(json.dumps(payload).encode())


class HybridFallbackTests(unittest.TestCase):
    def setUp(self):
        spec = importlib.util.spec_from_file_location("xc_hybrid_under_test", XC_PATH)
        self.xc = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.xc)
        self.home = tempfile.TemporaryDirectory(prefix="xc-hybrid-test-")
        self.addCleanup(self.home.cleanup)

    def run_main(self, transport, flags=(), mode="hybrid"):
        out, err = io.StringIO(), io.StringIO()
        argv = ["xc.py", "fixture", "parse", "-k", "2", "--mode", mode, *flags]
        state = {"corpus": "fixture", "prefix": PREFIX}
        # Every patched process-global value is restored, including urllib and
        # argv. A fresh field cache also keeps repeated subtests independent.
        with patch.object(self.xc, "ROOT", self.home.name), \
                patch.object(self.xc, "URL", "http://fixture.invalid"), \
                patch.object(self.xc, "_FIELDS_CACHE", {}), \
                patch.object(self.xc, "load_state", return_value=state), \
                patch.object(self.xc, "field_report_nudge"), \
                patch.object(self.xc.urllib.request, "urlopen", side_effect=transport), \
                patch.object(sys, "argv", argv), \
                contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            code = 0
            try:
                self.xc.main()
            except SystemExit as exc:
                code = exc.code
        return code, out.getvalue(), err.getvalue()

    def assert_fallback(self, transport, flags):
        code, out, err = self.run_main(transport, flags)
        self.assertEqual(code, 0, err)
        self.assertEqual(err, "")
        if "--json" in flags:
            self.assertEqual(json.loads(out), {"hits": {"hits": BM25_HITS[:2]}})
        else:
            self.assertIn("BM25 only", out)
            self.assertNotIn("hybrid RRF", out)
            self.assertNotIn("no index under", out)
            self.assertLess(out.index("fixture/alpha.rs"), out.index("fixture/beta.rs"))
            self.assertNotIn("fixture/gamma.rs", out)
            self.assertNotIn("fixture/delta.rs", out)
            if "--meatl" in flags:
                self.assertTrue(out.startswith("@mode BM25 only"))
                self.assertEqual(out.count("@ok "), 2)

    def test_optional_transport_failures_keep_bm25_in_each_output_mode(self):
        for stage in ("mapping", "vector"):
            for name, failure, during_read in FAILURES:
                for flags in ((), ("--json",), ("--meatl",)):
                    with self.subTest(stage=stage, failure=name, flags=flags):
                        transport = Transport(stage, failure, during_read)
                        self.assert_fallback(transport, flags)
                        self.assertEqual(transport.calls.count("bm25"), 1)
                        self.assertEqual(transport.calls.count("vector"), int(stage == "vector"))

    def test_optional_http_errors_still_fall_back(self):
        for stage in ("mapping", "vector"):
            with self.subTest(stage=stage):
                def failure():
                    error = urllib.error.HTTPError(
                        "http://fixture.invalid", 503, "fixture unavailable", {}, io.BytesIO(b"offline"))
                    self.addCleanup(error.close)
                    return error
                self.assert_fallback(Transport(stage, failure), ("--json",))

    def test_optional_http_error_with_unreadable_body_still_falls_back(self):
        for name, read_failure, _ in (FAILURES[1], FAILURES[4]):
            with self.subTest(failure=name):
                def failure():
                    error = urllib.error.HTTPError(
                        "http://fixture.invalid", 503, "fixture unavailable", {},
                        BrokenResponse(read_failure))
                    self.addCleanup(error.close)
                    return error
                self.assert_fallback(Transport("vector", failure), ("--json",))

    def test_primary_http_error_with_unreadable_body_is_still_fatal(self):
        for mode in ("bm25", "hybrid", "semantic"):
            for name, read_failure, _ in (FAILURES[1], FAILURES[4]):
                with self.subTest(mode=mode, failure=name):
                    def failure():
                        error = urllib.error.HTTPError(
                            "http://fixture.invalid", 503, "fixture unavailable", {},
                            BrokenResponse(read_failure))
                        self.addCleanup(error.close)
                        return error
                    stage = "vector" if mode == "semantic" else "bm25"
                    transport = Transport(stage, failure, semantic_only=mode == "semantic")
                    code, out, err = self.run_main(transport, ("--json",), mode)
                    self.assertEqual((code, out), (2, ""))
                    self.assertIn("search failed (503)", err)

    def test_primary_transport_failures_are_fatal_without_results(self):
        for mode in ("bm25", "hybrid"):
            for name, failure, during_read in FAILURES:
                with self.subTest(mode=mode, failure=name):
                    transport = Transport("bm25", failure, during_read)
                    code, out, err = self.run_main(transport, ("--json",), mode)
                    self.assertEqual(code, 2)
                    self.assertEqual(out, "")
                    self.assertIn("cannot reach XERJ", err)
                    self.assertNotIn("No passage", err)
                    self.assertNotIn("vector", transport.calls)

    def test_successful_hybrid_fuses_ranks_and_caps_results(self):
        transport = Transport()
        code, out, err = self.run_main(transport, ("--json",))
        self.assertEqual((code, err), (0, ""))
        hits = json.loads(out)["hits"]["hits"]
        self.assertEqual([h["_id"] for h in hits], ["beta", "alpha"])
        self.assertAlmostEqual(hits[0]["_score"], 1 / 62 + 1 / 61)
        self.assertEqual(hits[0]["_rrf_ranks"], {"0": 2, "1": 1})
        self.assertEqual(hits[0]["_source"], BM25_HITS[1]["_source"])
        self.assertEqual(transport.calls, ["loaded", "fields", "bm25", "mapping", "vector"])

    def test_empty_bm25_never_attempts_optional_retrieval(self):
        transport = Transport(bm25=[])
        code, out, err = self.run_main(transport)
        self.assertEqual((code, err), (1, ""))
        self.assertIn("No passage", out)
        self.assertEqual(transport.calls, ["loaded", "fields", "bm25"])

    def assert_semantic_failure(self, transport, flags):
        code, out, err = self.run_main(transport, flags, mode="semantic")
        self.assertEqual(code, 2, (out, err))
        self.assertEqual(out, "")
        self.assertIn("xc:", err)
        self.assertNotIn("No passage", err)
        self.assertNotIn("bm25", transport.calls)

    def test_standalone_semantic_transport_failures_are_fatal(self):
        for stage in ("mapping", "vector"):
            for name, failure, during_read in FAILURES:
                for flags in ((), ("--json",)):
                    with self.subTest(stage=stage, failure=name, flags=flags):
                        transport = Transport(stage, failure, during_read, semantic_only=True)
                        self.assert_semantic_failure(transport, flags)

    def test_standalone_semantic_http_errors_are_fatal(self):
        for stage in ("mapping", "vector"):
            for flags in ((), ("--json",)):
                with self.subTest(stage=stage, flags=flags):
                    def failure():
                        error = urllib.error.HTTPError(
                            "http://fixture.invalid", 503, "fixture unavailable", {}, io.BytesIO(b"offline"))
                        self.addCleanup(error.close)
                        return error
                    self.assert_semantic_failure(Transport(stage, failure, semantic_only=True), flags)

    def test_successful_standalone_semantic_preserves_vector_hits(self):
        transport = Transport(semantic_only=True)
        code, out, err = self.run_main(transport, ("--json",), mode="semantic")
        self.assertEqual((code, err), (0, ""))
        self.assertEqual(json.loads(out), {"hits": {"hits": VECTOR_HITS}})
        self.assertEqual(transport.calls, ["loaded", "mapping", "vector"])

    def test_standalone_semantic_genuine_misses_keep_exit_one(self):
        for capable in (False, True):
            with self.subTest(capable=capable):
                transport = Transport(semantic_only=True, capable=capable, vector=[])
                code, out, err = self.run_main(transport, mode="semantic")
                self.assertEqual((code, err), (1, ""))
                self.assertIn("No passage", out)
                expected = ["loaded", "mapping"] + (["vector"] if capable else [])
                self.assertEqual(transport.calls, expected)


if __name__ == "__main__":
    unittest.main()
