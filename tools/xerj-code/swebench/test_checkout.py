#!/usr/bin/env python3
"""Local-Git regression tests; no network, XERJ node, datasets or agent required."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

import gen_predictions


def git(repo, *args):
    return subprocess.run(["git", *args], cwd=repo, check=True,
                          capture_output=True, text=True).stdout.strip()


def commit(repo, content):
    (repo / "source.py").write_text(content)
    git(repo, "add", "source.py")
    git(repo, "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "fixture")
    return git(repo, "rev-parse", "HEAD")


class CheckoutTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.origin = self.root / "origin"
        self.origin.mkdir()
        git(self.origin, "init", "--quiet")
        git(self.origin, "config", "user.name", "Fixture")
        git(self.origin, "config", "user.email", "fixture@example.invalid")
        git(self.origin, "config", "core.hooksPath", ".git/hooks")
        self.base = commit(self.origin, "value = 'base'\n")
        config = self.root / "gitconfig"
        config.write_text(f'[url "{self.origin.as_uri()}"]\n    insteadOf = https://github.com/fixture/project\n')
        env = patch.dict(os.environ, {"GIT_CONFIG_GLOBAL": str(config), "GIT_CONFIG_NOSYSTEM": "1"})
        env.start()
        self.addCleanup(env.stop)

    def test_missing_commit_raises_instead_of_using_default_head(self):
        with self.assertRaises(subprocess.CalledProcessError):
            gen_predictions.clone_at("fixture/project", "missing-base-commit", str(self.root / "clone"))
        self.assertEqual(git(self.origin, "rev-parse", "HEAD"), self.base)

    def test_failed_clone_does_not_checkout_an_existing_destination(self):
        existing = self.root / "existing"
        git(self.root, "clone", "--quiet", str(self.origin), str(existing))
        git(existing, "config", "user.name", "Fixture")
        git(existing, "config", "user.email", "fixture@example.invalid")
        changed = commit(existing, "value = 'existing work'\n")
        with self.assertRaises(subprocess.CalledProcessError):
            gen_predictions.clone_at("fixture/project", self.base, str(existing))
        self.assertEqual(git(existing, "rev-parse", "HEAD"), changed)
        self.assertEqual((existing / "source.py").read_text(), "value = 'existing work'\n")

    def test_valid_checkout_uses_the_pinned_commit(self):
        commit(self.origin, "value = 'new default'\n")
        dest = self.root / "clone"
        gen_predictions.clone_at("fixture/project", self.base, str(dest))
        self.assertEqual(git(dest, "rev-parse", "HEAD"), self.base)
        self.assertEqual((dest / "source.py").read_text(), "value = 'base'\n")


if __name__ == "__main__":
    unittest.main()
