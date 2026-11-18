#!/usr/bin/env python3
"""Offline count oracles for local Cargo.lock and Debian source-index inputs."""
import gzip
from pathlib import Path
import subprocess
import tempfile
import unittest


class DebianCoverageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.lock = self.root / "Cargo.lock"
        self.index = self.root / "Sources.gz"
        self.lock.write_text('''version = 4

[[package]]
name = "serde"
version = "1.0.0"

[[package]]
name = "serde_derive"
version = "1.0.0"

[[package]]
name = "xerj-server"
version = "1.0.0"
''')
        self.script = Path(__file__).with_name("debian-rust-coverage.sh")

    def report(self, packages):
        text = "\n\n".join("Package: " + name + "\nVersion: 1.0" for name in packages)
        self.index.write_bytes(gzip.compress(text.encode()))
        return subprocess.run(["bash", str(self.script), "--lock", str(self.lock),
                               "--index", str(self.index)], capture_output=True, text=True, timeout=10)

    def assert_counts(self, packages, present, missing, percentage):
        result = self.report(packages)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertRegex(result.stdout, r"2 unique names")
        self.assertRegex(result.stdout, rf"packaged in Debian unstable \(rust-\*\):\s+{present} \({percentage}%\)")
        self.assertRegex(result.stdout, rf"NOT in Debian unstable:\s+{missing}")
        self.assertIn("packaged versions were NOT compared", result.stdout)

    def test_zero_matches_is_a_valid_measurement(self):
        self.assert_counts(["rust-unrelated"], 0, 2, "0.0")

    def test_partial_overlap_maps_underscore_names(self):
        self.assert_counts(["rust-serde-derive"], 1, 1, "50.0")

    def test_complete_overlap(self):
        self.assert_counts(["rust-serde", "rust-serde-derive"], 2, 0, "100.0")

    def test_empty_index_still_refuses_measurement(self):
        result = self.report([])
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no Package: entries", result.stderr)
        self.assertNotIn("packaged in Debian unstable", result.stdout)


if __name__ == "__main__":
    unittest.main()
