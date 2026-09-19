#!/usr/bin/env python3
"""Offline regressions for preserving query evidence in xc.py excerpts."""
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from unittest.mock import patch


XC_PATH = Path(__file__).resolve().parents[1] / "scripts" / "xc.py"
SPEC = importlib.util.spec_from_file_location("xc_passage_under_test", XC_PATH)
xc = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(xc)


def tail_match_body():
    # The only match fits in the first 800 characters, but its newline does not.
    return ("padding 123456789012\n" * 37
            + "needle(); more_source_text_here();\n"
            + "// trailing text\n" * 30)


class PassageWindowTests(unittest.TestCase):
    def window(self, body, query="needle", width=800):
        text, start, total = xc.best_window(body, query, width)
        self.assertEqual(total, len(body))
        self.assertGreaterEqual(start, 0)
        self.assertLessEqual(len(text), width)
        self.assertEqual(text, body[start:start + len(text)])
        return text, start

    def test_end_snap_keeps_the_matching_partial_line(self):
        text, _ = self.window(tail_match_body())
        self.assertIn("needle", text)

    def test_start_snap_does_not_jump_back_over_a_long_line(self):
        body = "// header\n" + "x" * 1500 + "needle" + "x" * 500
        text, _ = self.window(body)
        self.assertIn("needle", text)

    def test_alignment_preserves_all_selected_query_occurrences(self):
        body = ("needle\n" + "padding 123456789012\n" * 36
                + "padding abcde\nneedle(); more_source_text_here();\n"
                + "// trailing text\n" * 30)
        self.assertEqual(body[:800].count("needle"), 2)
        text, _ = self.window(body)
        self.assertEqual(text.count("needle"), 2)

    def test_lowercase_expansion_keeps_source_offsets(self):
        # U+0130 lowercases to two code points; source offsets must not shift.
        body = "\u0130" * 900 + "needle" + "x" * 1500
        text, _ = self.window(body)
        self.assertIn("needle", text)

    def test_readable_line_boundaries_when_the_match_is_preserved(self):
        body = "// padding line\n" * 70 + "needle();\n" + "// tail\n" * 100
        text, start = self.window(body)
        self.assertIn("needle", text)
        self.assertTrue(start == 0 or body[start - 1] == "\n")
        self.assertTrue(start + len(text) == len(body)
                        or body[start + len(text)] == "\n")

    def test_equal_scores_keep_the_first_candidate(self):
        body = "needle" + "x" * 1600 + "needle" + "x" * 1000
        text, start = self.window(body)
        self.assertEqual(start, 0)
        self.assertEqual(text, body[:800])

    def test_empty_and_short_bodies_are_returned_unchanged(self):
        for body in ("", "needle();\n", "x" * 800):
            with self.subTest(length=len(body)):
                self.assertEqual(self.window(body), (body, 0))

    def test_no_terms_and_no_matches_keep_the_head_fallback(self):
        body = "// plain source\n" * 100
        for query in ("", "$-1", "needle"):
            with self.subTest(query=query):
                self.assertEqual(self.window(body, query), (body[:800], 0))

    def test_custom_width_preserves_a_match_within_the_budget(self):
        body = "padding\n" * 3 + "needle(); more_source();\n" + "tail\n" * 30
        text, _ = self.window(body, width=32)
        self.assertIn("needle", text)

    def test_cli_emits_the_matching_source_with_and_without_symbols(self):
        body = tail_match_body()
        # Exercise the real argument parsing, HTTP request handling, and output
        # formatter without a live XERJ or changes to the user's corpus state.
        with tempfile.TemporaryDirectory(prefix="xc-passage-test-") as home:
            state = Path(home) / "state"
            state.mkdir()
            (state / "fixture.json").write_text(json.dumps({
                "corpus": "fixture", "prefix": "xc-fixture",
            }), encoding="utf-8")
            for extra in ([], ["--no-symbol"]):
                with self.subTest(extra=extra):
                    source = {"path": "acme/source.txt", "body": body}
                    if extra:
                        source["symbols"] = [
                            {"name": "needle", "kind": "fn", "line": 38},
                        ]

                    def urlopen(req, timeout=None):
                        if "/_cat/indices/" in req.full_url:
                            payload = [{"index": "xc-fixture-1"}]
                        elif req.full_url.endswith("/_mapping"):
                            payload = {"xc-fixture-1": {"mappings": {
                                "properties": {"body": {"type": "text"}},
                            }}}
                        elif req.full_url.endswith("/_search"):
                            payload = {"hits": {"hits": [{
                                "_source": source, "_score": 1.0,
                            }]}}
                        else:
                            self.fail(f"unexpected request: {req.full_url}")
                        return io.BytesIO(json.dumps(payload).encode())

                    out, err = io.StringIO(), io.StringIO()
                    with (patch.object(xc, "ROOT", home),
                          patch.dict(xc._FIELDS_CACHE, {}, clear=True),
                          patch.dict(os.environ, {"XERJ_CODE_NO_NUDGE": "1"}),
                          patch.object(xc.urllib.request, "urlopen", urlopen),
                          patch.object(sys, "argv", ["xc.py", "fixture", "needle"] + extra),
                          redirect_stdout(out), redirect_stderr(err)):
                        xc.main()
                    self.assertIn("needle", out.getvalue())
                    self.assertIn("1 passages from 'fixture'", out.getvalue())
                    self.assertEqual(err.getvalue(), "")


if __name__ == "__main__":
    unittest.main()
