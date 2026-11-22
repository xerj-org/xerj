#!/usr/bin/env python3
"""Regression tests for fetch_repo_stats.py. Stdlib only, no network. Run:

    python3 tools/corpus-hub-site/test_fetch_repo_stats.py

The fetcher crashed with KeyError: 'repos' because hub/candidates/ also holds
do-not-contact.json and owner-overrides.json, which have no "repos" list. These
tests pin that, and the other shapes a run can meet: a saved stats file with the
key missing or renamed, a corrupt partial file, a GraphQL reply with errors and
no data, and a rejected token.
"""
import contextlib
import io
import json
import pathlib
import sys
import tempfile
import types
import unittest
import urllib.error
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import fetch_repo_stats as f  # noqa: E402

SHA = "a" * 40


def fake_gen(hub, registry):
    return types.SimpleNamespace(HUB=pathlib.Path(hub), load_registry=lambda: registry)


class Quiet(unittest.TestCase):
    def setUp(self):
        self._err = contextlib.redirect_stderr(io.StringIO())
        self.err = self._err.__enter__()

    def tearDown(self):
        self._err.__exit__(None, None, None)


class TargetsTest(Quiet):
    def test_files_without_repos_are_skipped(self):
        with tempfile.TemporaryDirectory() as d:
            c = pathlib.Path(d, "candidates")
            (c / "authors").mkdir(parents=True)
            (c / "do-not-contact.json").write_text(json.dumps({"_doc": "x", "entries": [{"login": "a"}]}))
            (c / "owner-overrides.json").write_text(json.dumps({"_doc": "x", "overrides": {}}))
            (c / "broken.json").write_text("{not json")
            (c / "list.json").write_text("[1, 2]")
            (c / "repos-null.json").write_text(json.dumps({"repos": None}))
            (c / "good.json").write_text(json.dumps({"repos": [{"url": "https://github.com/a/b", "sha": SHA}, "junk"]}))
            (c / "authors" / "x.json").write_text(json.dumps({"login": "x"}))
            (c / "authors" / "y.json").write_text(json.dumps({"repos": [{"url": "https://github.com/y/z.git"}]}))
            registry = [{"corpus": "no-repos"}, {"corpus": "one", "repos": [{"url": "https://github.com/o/p", "sha": SHA, "files": 3}]}]
            with mock.patch.dict(sys.modules, {"gen": fake_gen(d, registry)}):
                want = f.targets()
        self.assertEqual(sorted(want), ["a/b", "o/p", "y/z"])
        self.assertTrue(want["a/b"]["tree"])
        self.assertFalse(want["o/p"]["tree"])
        self.assertIn("broken.json", self.err.getvalue())

    def test_real_hub_does_not_crash(self):
        want = f.targets()
        self.assertGreater(len(want), 0)


class SavedReposTest(unittest.TestCase):
    def test_shapes(self):
        self.assertEqual(f.saved_repos(None), {})
        self.assertEqual(f.saved_repos([]), {})
        self.assertEqual(f.saved_repos({"fetched": "x"}), {})
        self.assertEqual(f.saved_repos({"stats": {"a/b": {}}}), {})
        self.assertEqual(f.saved_repos({"repos": []}), {})
        self.assertEqual(f.saved_repos({"repos": {"a/b": {"stars": 1}, "c/d": 3}}), {"a/b": {"stars": 1}})


class QueryTest(Quiet):
    def test_errors_without_data(self):
        want = {"a/b": {"repo": "a/b", "sha": SHA}, "c/d": {"repo": "c/d", "sha": ""}}
        reply = {"errors": [{"type": "RATE_LIMITED", "message": "x", "path": None}, {"message": "no type"}]}
        done = {}
        with mock.patch.object(f, "post", return_value=reply):
            self.assertEqual(f.query("t", want, ["a/b", "c/d"], done), {})
        self.assertEqual(done, {})
        self.assertIn("RATE_LIMITED", self.err.getvalue())

    def test_not_found_marks_missing(self):
        want = {"a/b": {"repo": "a/b", "sha": ""}}
        reply = {"data": {"r0": None, "rateLimit": {"remaining": 9}}, "errors": [{"type": "NOT_FOUND", "path": ["r0"]}]}
        done = {}
        with mock.patch.object(f, "post", return_value=reply):
            self.assertEqual(f.query("t", want, ["a/b"], done), {"remaining": 9})
        self.assertEqual(done, {"a/b": {"repo": "a/b", "missing": True}})


def http_error(code):
    return urllib.error.HTTPError(f.API, code, "x", {}, None)


class PostTest(unittest.TestCase):
    def test_401_stops_at_once(self):
        calls = []

        def urlopen(*a, **k):
            calls.append(1)
            raise http_error(401)
        with mock.patch.object(f.urllib.request, "urlopen", urlopen), mock.patch.object(f.time, "sleep"):
            with self.assertRaises(f.AuthError):
                f.query("t", {"a/b": {"repo": "a/b", "sha": ""}, "c/d": {"repo": "c/d", "sha": ""}}, ["a/b", "c/d"], {})
        self.assertEqual(len(calls), 1)


class CoreRemainingTest(Quiet):
    def test_unexpected_reply_is_zero(self):
        body = io.BytesIO(json.dumps({"message": "Bad credentials"}).encode())
        with mock.patch.object(f.urllib.request, "urlopen", return_value=body):
            self.assertEqual(f.core_remaining("t"), 0)

    def test_http_error_is_zero(self):
        with mock.patch.object(f.urllib.request, "urlopen", side_effect=http_error(401)):
            self.assertEqual(f.core_remaining("t"), 0)


class MainTest(Quiet):
    def run_main(self, d, out_text, partial_text=None, graphql=None, argv=("x", "--no-trees")):
        out, partial = pathlib.Path(d, "repo_stats.json"), pathlib.Path(d, ".partial.json")
        if out_text is not None:
            out.write_text(out_text)
        if partial_text is not None:
            partial.write_text(partial_text)
        want = {"a/b": {"repo": "a/b", "sha": SHA, "tree": True}}

        def fill(tok, w, done):
            done["a/b"] = {"repo": "a/b", "stars": 5}
        with mock.patch.multiple(f, OUT=out, PARTIAL=partial, token=lambda: "t", targets=lambda: want,
                                 fetch_graphql=graphql or fill), mock.patch.object(sys, "argv", list(argv)):
            f.main()
        return out

    def test_old_file_without_repos_key(self):
        for text in ('{"fetched": "2026-01-01", "stats": {}}', "{broken", "[]", '{"repos": {"a/b": 7}}'):
            with self.subTest(text=text), tempfile.TemporaryDirectory() as d:
                out = self.run_main(d, text, partial_text='{"date": "x"}')
                self.assertEqual(json.loads(out.read_text())["repos"], {"a/b": {"repo": "a/b", "stars": 5}})

    def test_corrupt_partial_is_ignored(self):
        with tempfile.TemporaryDirectory() as d:
            out = self.run_main(d, None, partial_text="{trunc")
            self.assertEqual(json.loads(out.read_text())["repos"]["a/b"]["stars"], 5)

    def test_tree_kept_only_for_same_pin(self):
        old = {"repos": {"a/b": {"repo": "a/b", "tree": {"sha": SHA, "files": 2, "bytes": 9}}}}
        with tempfile.TemporaryDirectory() as d:
            out = self.run_main(d, json.dumps(old))
            self.assertEqual(json.loads(out.read_text())["repos"]["a/b"]["tree"]["files"], 2)
        old["repos"]["a/b"]["tree"] = {"sha": "b" * 40, "files": 2}
        with tempfile.TemporaryDirectory() as d:
            out = self.run_main(d, json.dumps(old))
            self.assertNotIn("tree", json.loads(out.read_text())["repos"]["a/b"])

    def test_bad_token_leaves_output_alone(self):
        def deny(tok, w, done):
            done["a/b"] = {"repo": "a/b"}
            raise f.AuthError("GitHub rejected the token (HTTP 401)")
        with tempfile.TemporaryDirectory() as d:
            with self.assertRaises(SystemExit) as cm:
                self.run_main(d, '{"repos": {}}', graphql=deny)
            self.assertIn("401", str(cm.exception.code))
            self.assertEqual(pathlib.Path(d, "repo_stats.json").read_text(), '{"repos": {}}')
            self.assertIn("a/b", json.loads(pathlib.Path(d, ".partial.json").read_text())["repos"])


if __name__ == "__main__":
    unittest.main()
