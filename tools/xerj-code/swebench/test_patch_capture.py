#!/usr/bin/env python3
"""Git patch round trips with a recording agent adapter, no paid model calls."""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import gen_predictions

_REAL_RUN = subprocess.run


def git(repo, *args, **kwargs):
    return _REAL_RUN(["git", *args], cwd=repo, check=True,
                     capture_output=True, text=True, **kwargs).stdout.strip()


class PatchCaptureTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "--quiet")
        git(self.repo, "config", "user.name", "Fixture")
        git(self.repo, "config", "user.email", "fixture@example.invalid")
        git(self.repo, "config", "core.hooksPath", ".git/hooks")
        (self.repo / "existing.py").write_text("value = 'before'\n")
        (self.repo / "removed.py").write_text("obsolete = True\n")
        (self.repo / ".gitignore").write_text("ignored.log\n")
        git(self.repo, "add", ".")
        git(self.repo, "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "fixture")

    def run_recording_agent(self, edit):
        def agent_or_git(cmd, **kwargs):
            if cmd[0] != "claude":
                return _REAL_RUN(cmd, **kwargs)
            edit()
            return subprocess.CompletedProcess(cmd, 0, json.dumps({"usage": {}, "modelUsage": {"fixture": {}}}), "")

        with patch.object(gen_predictions.subprocess, "run", agent_or_git):
            return gen_predictions.run_agent(str(self.repo), "Fixture issue", None, 5, 1)[0]

    def test_staged_new_and_deleted_source_round_trip_to_a_clean_base(self):
        clean = self.root / "clean"
        git(self.root, "clone", "--quiet", str(self.repo), str(clean))

        def edit():
            (self.repo / "existing.py").write_text("value = 'after'\n")
            git(self.repo, "add", "existing.py")
            (self.repo / "new_module.py").write_text("new_value = 42\n")
            (self.repo / "data.bin").write_bytes(b"\x00fixture\xff")
            (self.repo / "removed.py").unlink()

        diff = self.run_recording_agent(edit)
        git(clean, "apply", "--check", "-", input=diff)
        git(clean, "apply", "-", input=diff)
        self.assertEqual((clean / "existing.py").read_text(), "value = 'after'\n")
        self.assertEqual((clean / "new_module.py").read_text(), "new_value = 42\n")
        self.assertFalse((clean / "removed.py").exists())
        self.assertEqual((clean / "data.bin").read_bytes(), b"\x00fixture\xff")

    def test_agent_commits_are_compared_with_the_original_base(self):
        def edit():
            (self.repo / "existing.py").write_text("value = 'committed fix'\n")
            git(self.repo, "add", "existing.py")
            git(self.repo, "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "agent fixture")

        diff = self.run_recording_agent(edit)
        self.assertIn("value = 'committed fix'", diff)

    def test_ignored_agent_artifacts_remain_outside_the_prediction(self):
        def edit():
            (self.repo / "existing.py").write_text("value = 'after'\n")
            (self.repo / "ignored.log").write_text("fixture artifact\n")

        diff = self.run_recording_agent(edit)
        self.assertIn("value = 'after'", diff)
        self.assertNotIn("ignored.log", diff)

    def test_unchanged_agent_result_still_has_an_empty_patch(self):
        self.assertEqual(self.run_recording_agent(lambda: None), "")


if __name__ == "__main__":
    unittest.main()
