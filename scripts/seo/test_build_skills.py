#!/usr/bin/env python3
"""Public discovery regression tests; only temporary fixtures are mutated."""

import contextlib
import hashlib
import io
import json
import pathlib
import tempfile
import unittest
from unittest import mock

import build_skills as skills


class DiscoveryTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = pathlib.Path(tmp.name)
        self.landing = self.root / "landing"
        self.path = self.landing / skills.DISCOVERY / "xerj" / "SKILL.md"
        self.path.parent.mkdir(parents=True)
        self.path.write_text('---\nname: xerj\ndescription: "Find XERJ docs."\n---\n\n# Guide\n',
                             encoding="utf-8")
        self.index = self.path.parent.parent / "index.json"
        self.index.write_bytes(skills.render(self.landing))

    def run_cli(self, mode):
        with mock.patch.object(skills, "REPO_ROOT", self.root), \
                contextlib.redirect_stdout(io.StringIO()), \
                contextlib.redirect_stderr(io.StringIO()):
            return skills.main([mode])

    def test_published_index_matches_raw_artifact(self):
        landing = skills.REPO_ROOT / "landing"
        raw = (landing / skills.DISCOVERY / "index.json").read_bytes()
        self.assertEqual(raw, skills.render(landing))
        index = json.loads(raw)
        self.assertEqual(index["$schema"], skills.SCHEMA)
        self.assertEqual(len(index["skills"]), 1)
        entry = index["skills"][0]
        self.assertEqual(entry["type"], "skill-md")
        artifact = landing / entry["url"].lstrip("/")
        self.assertEqual(entry["digest"], "sha256:" + hashlib.sha256(artifact.read_bytes()).hexdigest())

    def test_stale_body_is_detected_without_rewriting_and_can_be_rebuilt(self):
        before = self.index.read_bytes()
        self.path.write_bytes(self.path.read_bytes() + b"\nAdditional instructions.\n")
        self.assertEqual(self.run_cli("--check"), 1)
        self.assertEqual(self.index.read_bytes(), before)
        self.assertEqual(self.run_cli("--write"), 0)
        self.assertNotEqual(self.index.read_bytes(), before)
        self.assertEqual(self.run_cli("--check"), 0)

    def test_metadata_is_derived_from_the_same_artifact(self):
        old = json.loads(self.index.read_bytes())["skills"][0]
        self.path.write_text(self.path.read_text().replace("Find XERJ docs.", "Read the endpoint contract."))
        entry = json.loads(skills.render(self.landing))["skills"][0]
        self.assertEqual(entry["description"], "Read the endpoint contract.")
        self.assertNotEqual(entry["digest"], old["digest"])
        self.assertEqual(self.run_cli("--check"), 1)

    def test_missing_output_check_does_not_create_it(self):
        self.index.unlink()
        self.assertEqual(self.run_cli("--check"), 1)
        self.assertFalse(self.index.exists())
        self.assertEqual(self.run_cli("--write"), 0)
        self.assertEqual(self.run_cli("--check"), 0)

    def test_missing_source_preserves_previous_index(self):
        before = self.index.read_bytes()
        self.path.unlink()
        self.assertEqual(self.run_cli("--write"), 1)
        self.assertEqual(self.index.read_bytes(), before)

    def test_invalid_metadata_preserves_previous_index(self):
        source = self.path.read_text()
        before = self.index.read_bytes()
        invalid = [source.replace("name: xerj", "name: ../xerj"),
                   source.replace("name: xerj", "name: other"),
                   source.replace("name: xerj", "name: x--y"),
                   source.replace("name: xerj", "name: " + "x" * 65),
                   source.replace('"Find XERJ docs."', '""'),
                   source.replace('"Find XERJ docs."', '"' + "x" * 1025 + '"'),
                   source.replace('"Find XERJ docs."', '"line one\\nline two"'),
                   source.replace('"Find XERJ docs."', '42'),
                   source.replace('"Find XERJ docs."', 'unquoted'),
                   source.replace("name: xerj", "name: xerj\nname: xerj"),
                   source.replace("name: xerj", "name: xerj\nunknown: value"),
                   source.split("\n\n")[0] + "\n",
                   source.replace("---\n", "", 1)]
        for text in invalid:
            with self.subTest(text=text[:100]):
                self.path.write_text(text)
                self.assertEqual(self.run_cli("--write"), 1)
                self.assertEqual(self.index.read_bytes(), before)

    def test_digest_uses_raw_bytes_not_normalized_text(self):
        raw = self.path.read_bytes().replace(b"\n", b"\r\n")
        self.path.write_bytes(raw)
        entry = skills.skill_entry(self.path)
        self.assertEqual(entry["digest"], "sha256:" + hashlib.sha256(raw).hexdigest())
        self.assertNotEqual(entry["digest"], "sha256:" + hashlib.sha256(raw.replace(b"\r\n", b"\n")).hexdigest())

    def test_internal_skills_are_not_automatically_published(self):
        internal = self.root / "tools" / "internal" / "SKILL.md"
        internal.parent.mkdir(parents=True)
        internal.write_text("Internal instructions are not a public skill.")
        extra = self.path.parent.parent / "unlisted" / "SKILL.md"
        extra.parent.mkdir()
        extra.write_text("Not explicitly selected for publication.")
        self.assertEqual(self.index.read_bytes(), skills.render(self.landing))


if __name__ == "__main__":
    unittest.main()
