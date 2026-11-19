#!/usr/bin/env python3
"""Regression tests: the scripts/bench repros refuse to report a number that
was not measured on the path the claim is about (#2075 items 11 and 12).

- query-latency-size0.sh must reject non-2xx `_search` answers instead of
  timing them as latency samples (an error page is fast, so it flatters p50).
- knn-recall.sh must refuse to score recall when GET /_cat/ann never reports
  a covering, non-stale graph: the queries would then measure the exact-scan
  fallback, not HNSW.

Both scripts run end to end against tests/fake_xerj.py (no engine build).
Needs bash, curl, node >= 18, python3.

    python3 scripts/bench/tests/test_bench_guards.py
"""
import os
import pathlib
import shutil
import subprocess
import unittest

HERE = pathlib.Path(__file__).resolve().parent
BENCH = HERE.parent
FAKE = HERE / "fake_xerj.py"


def run(script, **env):
    full = dict(os.environ)
    full.pop("PORT", None)
    full.update({"XERJ_BIN": str(FAKE)}, **env)
    return subprocess.run(["bash", str(BENCH / script)], env=full, capture_output=True,
                          text=True, timeout=300)


@unittest.skipUnless(shutil.which("node") and shutil.which("curl"), "needs node and curl")
class LatencyRejectsFailedRequests(unittest.TestCase):
    SMALL = {"DOCS": "20", "BATCH": "10", "ITERS": "20", "RATE": "2000", "WARMUP": "2"}

    def test_failed_search_is_not_a_latency_sample(self):
        r = run("query-latency-size0.sh", FAKE_SEARCH_STATUS="503", **self.SMALL)
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertNotIn("MEASURED  keep-alive client", r.stdout)
        self.assertIn("503", r.stderr + r.stdout)

    def test_successful_search_still_measures(self):
        r = run("query-latency-size0.sh", **self.SMALL)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("MEASURED  keep-alive client", r.stdout)


@unittest.skipUnless(shutil.which("node") and shutil.which("curl"), "needs node and curl")
class RecallRefusesBeforeAnnReady(unittest.TestCase):
    SMALL = {"VECS": "50", "DIMS": "4", "PROBES": "2", "ANN_WAIT_S": "2"}

    def assert_refused(self, r):
        self.assertNotEqual(r.returncode, 0, r.stdout)
        self.assertNotIn("MEASURED  recall", r.stdout)
        self.assertIn("ANN", r.stderr)

    def test_stale_graph_is_refused(self):
        self.assert_refused(run("knn-recall.sh", FAKE_ANN="stale", **self.SMALL))

    def test_partly_graphed_corpus_is_refused(self):
        self.assert_refused(run("knn-recall.sh", FAKE_ANN="partial", **self.SMALL))

    def test_missing_ann_row_is_refused(self):
        self.assert_refused(run("knn-recall.sh", FAKE_ANN="empty", **self.SMALL))

    def test_ready_graph_is_measured(self):
        r = run("knn-recall.sh", **self.SMALL)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("MEASURED  recall", r.stdout)


if __name__ == "__main__":
    unittest.main()
