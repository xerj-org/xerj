#!/usr/bin/env python3
"""Tests for owner_overrides.py (stdlib unittest; run directly or via CI)."""
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import owner_overrides as oo  # noqa: E402


def row(corpus, repo, commit="a" * 40, title="feat: thing", stars=3, name="N", owns=True):
    return {
        "corpus": corpus, "repo": repo, "repo_url": f"https://github.com/{repo}",
        "shelf": "AI", "topics": [],
        "contributor": {"login": repo.split("/")[0], "profile": f"https://github.com/{repo.split('/')[0]}",
                        "name": name, "owns_repo": owns},
        "consent": {"at": "2026-10-30T00:26Z", "scope": "the selected work"},
        "contribution": {"commit": commit, "commit_url": f"https://github.com/{repo}/commit/{commit}",
                         "title": title, "areas": ["src/x"], "langs": ["Python"]},
        "licence": {"spdx": "MIT"}, "pin": {"sha": commit, "to": "selected commit"},
        "repo_stars": stars,
    }


def manifest(corpus, repo, sha="a" * 40):
    return {"corpus": corpus, "domain": "d (selected work: src/x)", "status": "draft",
            "repos": [{"repo": repo.split("/")[1], "url": f"https://github.com/{repo}", "sha": sha,
                       "licence": "MIT", "review": {"note": "n"}}]}


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.a = self.tmp / "authors"
        self.a.mkdir()
        self.write_generated()

    def tearDown(self):
        shutil.rmtree(self.tmp)

    def write_generated(self):
        """What the pipeline emits: the pre-request state."""
        for p in self.a.iterdir():
            p.unlink()
        rows = [row("own--keep", "own/keep"), row("old--slug", "old/slug", stars=1),
                row("other--x", "other/x", title="a | b")]
        for r in rows[:2]:
            m = manifest(r["corpus"], r["repo"])
            (self.a / f"{r['corpus']}.json").write_text(oo.dump_manifest(m), encoding="utf-8")
        (self.a / "CONTRIBUTORS.jsonl").write_text(
            "\n".join(oo.dump_row(r) for r in rows) + "\n", encoding="utf-8")
        md = ["# Contributors", "", "## AI", "", "| Repo | Contributor | Consent | Contribution | Licence | Stars |",
              "|---|---|---|---|---|---|"] + [oo.render_md_row(r) for r in rows]
        (self.a / "CONTRIBUTORS.md").write_text("\n".join(md) + "\n", encoding="utf-8")

    def overrides(self):
        data = {"overrides": [
            {"corpus": "own--keep", "issue": 1,
             "manifest": {"domain": "d", "repos.0.sha": "b" * 40},
             "ledger": {"pin.sha": "b" * 40, "pin.to": "default-branch head (whole repository)",
                        "consent.scope": "the whole repository", "contribution.commit": None,
                        "contribution.commit_url": None, "contribution.title": None,
                        "contribution.areas": [], "contribution.langs": []}},
            {"corpus": "new--slug", "issue": 2, "replaces": "old--slug",
             "manifest": {"repos.0.repo": "slug", "repos.0.url": "https://github.com/new/slug"},
             "ledger": {"repo": "new/slug", "repo_url": "https://github.com/new/slug",
                        "contributor.login": "new", "contributor.profile": "https://github.com/new",
                        "contribution.commit_url": "https://github.com/new/slug/commit/" + "a" * 40,
                        "repo_stars": 0}},
        ]}
        p = self.tmp / "owner-overrides.json"
        p.write_text(json.dumps(data), encoding="utf-8")
        return p

    def run_main(self, *args):
        return oo.main(["--authors", str(self.a), "--overrides", str(self.overrides()), *args])

    def rows(self):
        return {json.loads(l)["corpus"]: json.loads(l)
                for l in (self.a / "CONTRIBUTORS.jsonl").read_text(encoding="utf-8").splitlines()}


class TestOwnerOverrides(Fixture):
    def test_check_fails_on_generated_output(self):
        self.assertEqual(self.run_main("--check"), 1)

    def test_apply_then_check_passes_and_is_idempotent(self):
        self.assertEqual(self.run_main(), 0)
        snap = {p.name: p.read_text(encoding="utf-8") for p in self.a.iterdir()}
        self.assertEqual(self.run_main("--check"), 0)
        self.assertEqual(self.run_main(), 0)
        self.assertEqual(snap, {p.name: p.read_text(encoding="utf-8") for p in self.a.iterdir()})

    def test_whole_repo_and_pin(self):
        self.run_main()
        m = json.loads((self.a / "own--keep.json").read_text(encoding="utf-8"))
        self.assertEqual(m["repos"][0]["sha"], "b" * 40)
        self.assertEqual(m["domain"], "d")
        r = self.rows()["own--keep"]
        self.assertIsNone(r["contribution"]["commit"])
        self.assertEqual(r["pin"]["sha"], "b" * 40)
        md = (self.a / "CONTRIBUTORS.md").read_text(encoding="utf-8")
        self.assertIn("| [own/keep](https://github.com/own/keep) | [@own](https://github.com/own) N (owner)"
                      " | 2026-10-30T00:26Z | repo head | MIT | 3 |", md)

    def test_rename_replaces_old_slug_everywhere(self):
        self.run_main()
        self.assertFalse((self.a / "old--slug.json").exists())
        m = json.loads((self.a / "new--slug.json").read_text(encoding="utf-8"))
        self.assertEqual(m["corpus"], "new--slug")
        self.assertEqual(m["repos"][0]["url"], "https://github.com/new/slug")
        rows = self.rows()
        self.assertNotIn("old--slug", rows)
        self.assertEqual(rows["new--slug"]["repo"], "new/slug")
        md = (self.a / "CONTRIBUTORS.md").read_text(encoding="utf-8")
        self.assertNotIn("old/slug", md)
        self.assertIn("| [new/slug](https://github.com/new/slug) |", md)

    def test_survives_a_refresh(self):
        """Pipeline regenerates the old state; re-applying restores the request."""
        self.run_main()
        want = {p.name: p.read_text(encoding="utf-8") for p in self.a.iterdir()}
        self.write_generated()  # the hourly refresh
        self.assertEqual(self.run_main("--check"), 1)
        self.run_main()
        self.assertEqual(want, {p.name: p.read_text(encoding="utf-8") for p in self.a.iterdir()})

    def test_untouched_entries_stay_byte_identical(self):
        before = self.rows()["other--x"]
        self.run_main()
        self.assertEqual(before, self.rows()["other--x"])
        self.assertIn("a \\| b", (self.a / "CONTRIBUTORS.md").read_text(encoding="utf-8"))

    def test_dropped_entry_is_not_resurrected(self):
        """An owner opt-out removed by the pipeline must not come back via an override."""
        (self.a / "own--keep.json").unlink()
        self.run_main()
        self.assertFalse((self.a / "own--keep.json").exists())


class TestRealRegistry(unittest.TestCase):
    def test_overrides_file_loads(self):
        entries = oo.load_overrides()
        self.assertTrue(entries)
        for e in entries:
            self.assertIsInstance(e["issue"], int)

    def test_md_renderer_matches_every_generated_row(self):
        """render_md_row reproduces the pipeline's CONTRIBUTORS.md rows."""
        md = set((oo.AUTHORS / "CONTRIBUTORS.md").read_text(encoding="utf-8").splitlines())
        rows = [json.loads(l) for p in sorted(oo.AUTHORS.glob("CONTRIBUTORS*.jsonl"))
                for l in p.read_text(encoding="utf-8").splitlines() if l.strip()]
        missing = [r["corpus"] for r in rows if oo.render_md_row(r) not in md]
        self.assertEqual(missing, [])


if __name__ == "__main__":
    unittest.main()
