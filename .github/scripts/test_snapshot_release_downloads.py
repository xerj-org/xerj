#!/usr/bin/env python3
"""Offline filesystem regressions for the release-download snapshot script."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class SnapshotHistoryTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.output = self.root / "history with spaces.jsonl"
        self.bin = self.root / "bin"
        self.bin.mkdir()
        jq = shutil.which("jq")
        if not jq:
            self.skipTest("jq is required by the production snapshot script")
        (self.bin / "jq").symlink_to(jq)
        self.write_tool("date", '''#!/bin/sh
case "$*" in
  '-u +%F') printf '2026-10-21\\n';;
  '-u +%FT%TZ') printf '2026-10-21T06:17:00Z\\n';;
  *) exit 97;;
esac
''')
        self.write_tool("gh", '''#!/bin/sh
case "$*" in
  *'/releases?per_page=100'*)
    if [ "${BAD_RELEASES:-0}" = 1 ]; then printf '{invalid}\\n'; exit 0; fi
    printf '%s\\n' '[{"tag_name":"v1.0.0","assets":[{"name":"xerj.tar.gz","download_count":5},{"name":"xerj.tar.gz.sha256","download_count":3}]}]';;
  *'/traffic/clones'|*'/traffic/views') printf '{"count":3,"uniques":2}\\n';;
  *) exit 98;;
esac
''')
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ.get("PATH", ""))
        self.env.pop("GH_TOKEN", None)
        self.script = Path(__file__).with_name("snapshot-release-downloads.sh")

    def write_tool(self, name, text):
        path = self.bin / name
        path.write_text(text)
        path.chmod(0o755)

    def run_snapshot(self):
        return subprocess.run(["bash", str(self.script), "--out", str(self.output)],
                              env=self.env, capture_output=True, text=True, timeout=10)

    def test_initial_snapshot_and_same_day_replacement(self):
        for _ in range(2):
            result = self.run_snapshot()
            self.assertEqual(result.returncode, 0, result.stderr)
        rows = [json.loads(line) for line in self.output.read_text().splitlines()]
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["date"], "2026-10-21")
        self.assertEqual(rows[0]["totals"], {"releases": 1, "binary": 5, "checksum": 3, "all": 8})

    def test_valid_prior_days_survive_replacement(self):
        old = {"date": "2026-10-20", "retained": "older evidence"}
        self.output.write_text(json.dumps(old) + '\n{"date":"2026-10-21","old":true}\n')
        result = self.run_snapshot()
        self.assertEqual(result.returncode, 0, result.stderr)
        rows = [json.loads(line) for line in self.output.read_text().splitlines()]
        self.assertEqual(rows[0], old)
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[1]["totals"]["all"], 8)

    def test_malformed_history_is_preserved_until_repaired(self):
        before = b'{"date":"2026-10-19"}\n{invalid}\n{"date":"2026-10-20"}\n'
        self.output.write_bytes(before)
        result = self.run_snapshot()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.output.read_bytes(), before)
        self.assertIn("parse error", result.stderr.lower())
        self.output.write_bytes(before.replace(b'{invalid}\n', b''))
        repaired = self.run_snapshot()
        self.assertEqual(repaired.returncode, 0, repaired.stderr)
        self.assertEqual(len(self.output.read_text().splitlines()), 3)

    def test_invalid_api_payload_preserves_existing_history(self):
        before = b'{"date":"2026-10-20","retained":true}\n'
        self.output.write_bytes(before)
        self.env["BAD_RELEASES"] = "1"
        result = self.run_snapshot()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.output.read_bytes(), before)


if __name__ == "__main__":
    unittest.main()
