#!/usr/bin/env python3
"""Regression test for ask_arm.cmd_ask macro-F1 (#2075 item 16).

Invalid plans (score None) must count as 0.0 in the macro-F1 average
instead of being dropped from it. Stdlib only; no engine is needed.
"""
import contextlib
import io
import json
import pathlib
import tempfile
import unittest
from unittest import mock

import ask_arm


def make_pair(pid, doc_ids):
    return {
        "id": pid,
        "prompt": f"prompt-{pid}",
        "gold": {"index": "ax-test", "doc_ids": doc_ids},
    }


class MacroF1AllPairs(unittest.TestCase):
    def run_ask(self, pair_list, ask_responses, search_results):
        """Run cmd_ask with everything network-related mocked."""

        def fake_ask_once(prompt, index):
            if prompt.startswith("probe"):
                return 200, {}, 0.0
            code, resp = ask_responses[prompt]
            return code, resp, 1.0

        def fake_search_ids(index, query):
            return search_results[json.dumps(query, sort_keys=True)]

        with tempfile.TemporaryDirectory() as tmp:
            with mock.patch.object(ask_arm, "pairs", return_value=pair_list), \
                 mock.patch.object(ask_arm, "ask_once", side_effect=fake_ask_once), \
                 mock.patch.object(ask_arm, "search_ids", side_effect=fake_search_ids), \
                 contextlib.redirect_stdout(io.StringIO()):
                rc = ask_arm.cmd_ask(tmp, 1)
            self.assertEqual(rc, 0)
            return json.loads((pathlib.Path(tmp) / "summary.json").read_text())

    def test_invalid_plan_counts_as_zero(self):
        good_q = {"term": {"ok": 1}}
        bad_q = {"bad": 1}
        summary = self.run_ask(
            [make_pair("good", ["d1", "d2"]), make_pair("bad", ["d3"])],
            {
                "prompt-good": (200, {"query": good_q}),
                "prompt-bad": (200, {"query": bad_q}),
            },
            {
                json.dumps(good_q, sort_keys=True): ({"d1", "d2"}, 200, (2, 2)),
                # engine rejects the DSL with HTTP 400 -> invalid plan
                json.dumps(bad_q, sort_keys=True): (None, 400, {"error": "bad dsl"}),
            },
        )
        self.assertEqual(summary["pairs"], 2)
        self.assertEqual(summary["invalid_dsl_out"], 1)
        # one perfect plan (1.0) + one invalid plan (0.0) over ALL pairs = 0.5
        self.assertEqual(summary["macro_f1"], 0.5)

    def test_all_valid_plans_unchanged(self):
        q1, q2 = {"term": {"a": 1}}, {"term": {"b": 2}}
        summary = self.run_ask(
            [make_pair("p1", ["d1"]), make_pair("p2", ["d2"])],
            {
                "prompt-p1": (200, {"query": q1}),
                "prompt-p2": (200, {"query": q2}),
            },
            {
                json.dumps(q1, sort_keys=True): ({"d1"}, 200, (1, 1)),
                json.dumps(q2, sort_keys=True): ({"d2"}, 200, (1, 1)),
            },
        )
        self.assertEqual(summary["invalid_dsl_out"], 0)
        self.assertEqual(summary["macro_f1"], 1.0)


if __name__ == "__main__":
    unittest.main()