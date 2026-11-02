#!/usr/bin/env python3
"""Tests for strip_author_grants.py (stdlib unittest; run directly or via CI)."""
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "strip_author_grants.py"

GRANT_ROW = {
    "corpus": "x--y", "repo": "x/y", "repo_url": "https://github.com/x/y",
    "shelf": "AI", "repo_stars": 2,
    "contributor": {"login": "x"}, "consent": {"scope": "the selected work"},
    "licence": {"spdx": "AGPL-3.0", "suggested_use": "approach-only",
                "author_grant": {"terms": "MIT", "scope": "the author's own changes",
                                 "commit": "b" * 40, "at": "2026-10-30T19:13Z",
                                 "receipt": "5b381aa78f32"}},
    "pin": {"sha": "b" * 40, "to": "selected commit"},
}
CLEAN_ROW = {
    "corpus": "p--q", "repo": "p/q", "repo_url": "https://github.com/p/q",
    "shelf": "AI", "repo_stars": 4,
    "contributor": {"login": "p"}, "consent": {"scope": "the selected work"},
    "licence": {"spdx": "MIT", "suggested_use": "adapt-with-attribution"},
    "pin": {"sha": "a" * 40, "to": "selected commit"},
}
# The pipeline's own pretty format for a manifest (custom indentation, kept
# byte-exact for untouched files): the block is flat, one key per line.
MANIFEST = """{
 "corpus": "x--y",
 "domain": "d",
 "repos": [
  {
   "url": "https://github.com/x/y",
   "sha": "%s",
   "licence": "AGPL-3.0",
   "review": {
    "spdx": "AGPL-3.0",
    "use": "approach-only",
    "note": "Inclusion consented; used under that licence with attribution.",
    "author_grant": {
     "terms": "MIT",
     "receipt": "5b381aa78f32"
    }
   }
  }
 ]
}
""" % ("b" * 40)


class StripTest(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.authors = self.tmp / "authors"
        self.authors.mkdir()
        (self.authors / "CONTRIBUTORS.jsonl").write_text(
            json.dumps(GRANT_ROW) + "\n" + json.dumps(CLEAN_ROW) + "\n", encoding="utf-8")
        (self.authors / "x--y.json").write_text(MANIFEST, encoding="utf-8")
        self.orig_ledger = (self.authors / "CONTRIBUTORS.jsonl").read_text(encoding="utf-8")
        self.orig_manifest = MANIFEST

    def run_strip(self):
        return subprocess.run([sys.executable, str(SCRIPT), "--authors", str(self.authors)], check=True, capture_output=True, text=True)

    def test_strips_both_shapes_and_keeps_untouched_bytes(self):
        out = self.run_strip()
        self.assertIn("1 ledger row(s), 1 manifest(s)", out.stdout)
        rows = [json.loads(l) for l in (self.authors / "CONTRIBUTORS.jsonl").read_text().splitlines()]
        self.assertNotIn("author_grant", rows[0]["licence"])
        self.assertEqual(rows[0]["licence"]["spdx"], "AGPL-3.0")  # rest of the row survives
        self.assertEqual(rows[1], CLEAN_ROW)  # untouched row re-parsed identically
        man = (self.authors / "x--y.json").read_text(encoding="utf-8")
        self.assertNotIn("author_grant", man)
        self.assertIn('"note": "Inclusion consented; used under that licence with attribution."', man)
        self.assertIn('"use": "approach-only"', man)

    def test_idempotent(self):
        self.run_strip()
        once = (self.authors / "x--y.json").read_text(encoding="utf-8")
        out = self.run_strip()
        self.assertIn("0 ledger row(s), 0 manifest(s)", out.stdout)
        self.assertEqual(once, (self.authors / "x--y.json").read_text(encoding="utf-8"))

    def test_unrecognized_shape_fails_loudly(self):
        (self.authors / "x--y.json").write_text(
            MANIFEST.replace('"terms": "MIT",\n     "receipt"', '"terms": {"nested": "MIT"},\n     "receipt"'),
            encoding="utf-8")
        proc = subprocess.run([sys.executable, str(SCRIPT), "--authors", str(self.authors)], capture_output=True, text=True)
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("unrecognized shape", proc.stderr)


if __name__ == "__main__":
    unittest.main()
