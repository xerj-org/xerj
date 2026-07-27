import json
import pathlib
import stat
import subprocess
import sys
import tempfile
import unittest


HERE = pathlib.Path(__file__).resolve().parent


class CaptureToolingTests(unittest.TestCase):
    def test_missing_requested_profile_is_machine_readable_failure(self):
        with tempfile.TemporaryDirectory() as parent:
            output = pathlib.Path(parent) / "capture"
            result = subprocess.run(
                [
                    sys.executable,
                    str(HERE / "capture.py"),
                    "--output",
                    str(output),
                    "--cpu-seconds",
                    "1",
                    "--workload",
                    "negative-smoke",
                    "--corpus",
                    "none",
                    "--concurrency",
                    "1",
                    "--cache-state",
                    "cold",
                    "--build-features",
                    "debug-profiling",
                    "--build-profile",
                    "debug",
                    "--",
                    "/usr/bin/sleep",
                    "0.01",
                ],
                check=False,
            )
            self.assertEqual(result.returncode, 2)
            manifest_path = output / "manifest.json"
            manifest = json.loads(manifest_path.read_text())
            self.assertEqual(manifest["status"], "profile_failed")
            self.assertEqual(manifest["missing_requested_artifacts"], ["cpu.pb"])
            self.assertIn("server.log", manifest["artifacts"])
            self.assertIn("git_head", manifest["source"])
            self.assertEqual(
                stat.S_IMODE(manifest_path.stat().st_mode),
                0o600,
            )

            inspect = subprocess.run(
                [sys.executable, str(HERE / "inspect.py"), str(output)],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertEqual(inspect.returncode, 1)
            self.assertFalse(json.loads(inspect.stdout)["valid"])

    def test_stop_after_requires_publication_grace(self):
        result = subprocess.run(
            [
                sys.executable,
                str(HERE / "capture.py"),
                "--output",
                "/unused",
                "--cpu-seconds",
                "10",
                "--delay-seconds",
                "5",
                "--stop-after",
                "15",
                "--workload",
                "unused",
                "--corpus",
                "unused",
                "--concurrency",
                "1",
                "--cache-state",
                "cold",
                "--build-features",
                "debug-profiling",
                "--build-profile",
                "profiling",
                "--",
                "/usr/bin/true",
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 2)
        self.assertIn("after capture", result.stderr)


if __name__ == "__main__":
    unittest.main()
