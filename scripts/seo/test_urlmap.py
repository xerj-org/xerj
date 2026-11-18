#!/usr/bin/env python3
"""Date-source regressions. Run: python3 scripts/seo/test_urlmap.py."""

import datetime as dt
import os
import pathlib
import random
import subprocess
import tempfile
import unittest
from unittest import mock
import xml.etree.ElementTree as ET

import gen_sitemap
import urlmap


class DateSourceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.repo = self.root / "source"
        self.git("init", str(self.repo))
        self.git("-C", str(self.repo), "config", "user.name", "Date test")
        self.git("-C", str(self.repo), "config", "user.email", "date@example.invalid")
        self.page = self.repo / "page.html"
        self.page.write_text("original page", encoding="utf-8")
        self.commit("page", "2020-01-02T12:00:00+00:00")
        (self.repo / "other.txt").write_text("unrelated", encoding="utf-8")
        self.commit("unrelated", "2021-03-04T12:00:00+00:00")

    @staticmethod
    def git(*args, env=None):
        return subprocess.run(["git", *args], check=True, capture_output=True,
                              text=True, env=env).stdout.strip()

    def commit(self, message, date):
        self.git("-C", str(self.repo), "add", ".")
        env = {**os.environ, "GIT_AUTHOR_DATE": date, "GIT_COMMITTER_DATE": date}
        self.git("-C", str(self.repo), "commit", "-m", message, env=env)

    def test_full_history_uses_actual_page_commit(self):
        dates = urlmap.DateSource(self.repo)
        self.assertEqual(dates.last_modified(self.page), "2020-01-02")
        self.assertEqual(dates.first_published(self.page), "2020-01-02")
        self.assertEqual(dates.fallbacks, [])

    def test_dates_are_batched_for_all_paths_and_both_ends(self):
        pages = [self.repo / name for name in
                 ("space name.html", "2024-01-01.html", "caf\u00e9.html")]
        for page in pages:
            page.write_text("first", encoding="utf-8")
        self.commit("add pages", "2023-01-02T00:30:00+02:00")
        for page in pages:
            page.write_text("second", encoding="utf-8")
        # Traversal order, not min/max timestamp: this newer commit is backdated.
        self.commit("edit pages", "2022-03-04T23:30:00-02:00")
        with mock.patch.object(urlmap.subprocess, "run", wraps=subprocess.run) as run:
            dates = urlmap.DateSource(self.repo)
            for page in pages:
                for _ in range(2):
                    self.assertEqual(dates.first_published(page), "2023-01-01")
                    self.assertEqual(dates.last_modified(page), "2022-03-05")
            # Shallow/config probes plus one history read, independent of pages.
            self.assertEqual(run.call_count, 3)

    def test_history_failure_is_cached_and_records_fallbacks(self):
        dates = urlmap.DateSource(self.repo)
        with mock.patch.object(urlmap.subprocess, "run",
                               side_effect=subprocess.TimeoutExpired("git", 30)) as run:
            self.assertEqual(dates.last_modified(self.page), dates._mtime_date(self.page))
            self.assertEqual(dates.first_published(self.page), dates._mtime_date(self.page))
            self.assertEqual(run.call_count, 1)
        self.assertEqual(dates.fallbacks, [str(self.page), str(self.page)])

    def test_merge_discarded_branch_does_not_supply_page_dates(self):
        branch = self.git("-C", str(self.repo), "branch", "--show-current")
        self.git("-C", str(self.repo), "checkout", "-b", "discarded")
        self.page.write_text("discarded", encoding="utf-8")
        self.commit("discarded page edit", "2024-01-01T12:00:00+00:00")
        self.git("-C", str(self.repo), "checkout", branch)
        self.git("-C", str(self.repo), "merge", "-s", "ours", "--no-edit", "discarded")
        with mock.patch.object(urlmap.subprocess, "run", wraps=subprocess.run) as run:
            dates = urlmap.DateSource(self.repo)
            self.assertEqual(dates.last_modified(self.page), "2020-01-02")
            self.assertEqual(dates.first_published(self.page), "2020-01-02")
            self.assertEqual(dates.last_modified(self.repo / "other.txt"), "2021-03-04")
            # All merge-parent comparisons share a single diff-tree process.
            self.assertEqual(run.call_count, 4)

    def test_inherited_rename_following_preserves_first_publication(self):
        renamed = self.repo / "renamed.html"
        self.git("-C", str(self.repo), "mv", "page.html", "renamed.html")
        self.commit("rename page", "2024-02-03T12:00:00+00:00")
        self.git("-C", str(self.repo), "config", "log.follow", "true")
        with mock.patch.object(urlmap.subprocess, "run", wraps=subprocess.run) as run:
            dates = urlmap.DateSource(self.repo)
            for _ in range(2):
                self.assertEqual(dates.first_published(renamed), "2020-01-02")
                self.assertEqual(dates.last_modified(renamed), "2024-02-03")
            # Probes and one cached per-file read for this optional Git mode.
            self.assertEqual(run.call_count, 3)

    def test_unreadable_follow_config_leaves_date_semantics_to_git(self):
        real_run = subprocess.run

        def unreadable_config(args, **kwargs):
            if "config" in args:
                return subprocess.CompletedProcess(args, 128, stdout=b"", stderr=b"failure")
            return real_run(args, **kwargs)

        with mock.patch.object(urlmap.subprocess, "run", side_effect=unreadable_config) as run:
            dates = urlmap.DateSource(self.repo)
            self.assertEqual(dates.first_published(self.page), "2020-01-02")
            self.assertEqual(dates.last_modified(self.page), "2020-01-02")
            self.assertEqual(run.call_count, 3)
            self.assertEqual(dates._history, {})

    def test_generated_merge_histories_match_git(self):
        # Real Git is the oracle: include equal/backdated timestamps, octopus
        # merges, second-parent selections, deletions, and re-added paths.
        rng = random.Random(3226)

        def write_object(command, data, *args, env=None):
            return subprocess.run(
                ["git", "-C", str(self.repo), command, *args], input=data,
                capture_output=True, check=True, env=env).stdout.decode().strip()

        blobs = [write_object("hash-object", f"value {i}".encode(), "-w", "--stdin")
                 for i in range(4)]
        names = [f"file-{i}.html" for i in range(4)]
        commits = []
        trees = []
        for i in range(24):
            parents = rng.sample(range(i), min(i, rng.choice([1, 1, 2, 3])))
            tree = {}
            for name in names:
                choices = [trees[p].get(name) for p in parents] + [None, *blobs]
                value = rng.choice(choices)
                if value:
                    tree[name] = value
            tree_id = write_object("mktree", "".join(
                f"100644 blob {oid}\t{name}\n" for name, oid in sorted(tree.items())).encode())
            date = f"2020-01-{rng.randrange(1, 8):02d}T12:00:00+00:00"
            env = {**os.environ, "GIT_AUTHOR_DATE": date, "GIT_COMMITTER_DATE": date}
            args = [tree_id]
            for p in parents:
                args.extend(["-p", commits[p]])
            commits.append(write_object("commit-tree", f"commit {i}".encode(), *args, env=env))
            trees.append(tree)
        for rev in commits[-8:]:
            self.git("-C", str(self.repo), "update-ref", "HEAD", rev)
            dates = urlmap.DateSource(self.repo)
            for name in names:
                with self.subTest(revision=rev, path=name):
                    days = self.git("-C", str(self.repo), "log",
                                    "--date=format-local:%Y-%m-%d", "--format=%cd",
                                    "--", name, env={**os.environ, "TZ": "UTC"}).splitlines()
                    self.assertEqual(dates._git_date(pathlib.Path(name)), days[0] if days else None)
                    self.assertEqual(dates._git_date(self.repo / name, first=True),
                                     days[-1] if days else None)

    def test_failed_merge_batch_does_not_publish_partial_history(self):
        branch = self.git("-C", str(self.repo), "branch", "--show-current")
        self.git("-C", str(self.repo), "checkout", "-b", "side")
        (self.repo / "side.html").write_text("side", encoding="utf-8")
        self.commit("side", "2024-01-01T12:00:00+00:00")
        self.git("-C", str(self.repo), "checkout", branch)
        self.git("-C", str(self.repo), "merge", "--no-edit", "--no-ff", "side")
        dates = urlmap.DateSource(self.repo)
        real_run = subprocess.run

        def fail_diff(args, **kwargs):
            if "diff-tree" in args:
                return subprocess.CompletedProcess(args, 1, stdout=b"", stderr=b"failure")
            return real_run(args, **kwargs)

        with mock.patch.object(urlmap.subprocess, "run", side_effect=fail_diff) as run:
            self.assertEqual(dates.last_modified(self.page), dates._mtime_date(self.page))
            self.assertEqual(dates.first_published(self.page), dates._mtime_date(self.page))
            self.assertEqual(run.call_count, 3)
        self.assertEqual(dates.fallbacks, [str(self.page), str(self.page)])

    def test_nul_records_preserve_unusual_filenames(self):
        raw = (b"\0header\0\nspace name.html\0\nleading-newline.html\0"
               b"quote\".html\0\0empty\0\0last\0\n2024-01-01.html\0")
        self.assertEqual(list(urlmap.DateSource._history_records(raw)), [
            ("header", {"space name.html", "\nleading-newline.html", 'quote".html'}),
            ("empty", set()), ("last", {"2024-01-01.html"})])

    def test_shallow_history_records_mtime_fallback(self):
        shallow = self.root / "shallow"
        self.git("clone", "--depth", "1", self.repo.as_uri(), str(shallow))
        page = shallow / "page.html"
        stamp = dt.datetime(2022, 5, 6, 12, tzinfo=dt.timezone.utc).timestamp()
        os.utime(page, (stamp, stamp))
        dates = urlmap.DateSource(shallow)
        self.assertEqual(dates.last_modified(page), "2022-05-06")
        self.assertEqual(dates.first_published(page), "2022-05-06")
        self.assertIn(str(page), dates.fallbacks)
        sitemap = ET.fromstring(gen_sitemap.build(shallow, dates))
        self.assertEqual(sitemap.findtext(".//{*}lastmod"), "2022-05-06")

    def test_untracked_page_records_mtime_fallback(self):
        page = self.repo / "new.html"
        page.write_text("untracked", encoding="utf-8")
        dates = urlmap.DateSource(self.repo)
        self.assertEqual(dates.last_modified(page), dates._mtime_date(page))
        self.assertEqual(dates.fallbacks, [str(page)])

    def test_no_repository_records_mtime_fallback(self):
        dates = urlmap.DateSource(self.root)
        self.assertFalse(dates.git_ok)
        self.assertEqual(dates.last_modified(self.page), dates._mtime_date(self.page))
        self.assertEqual(dates.fallbacks, [str(self.page)])

    def test_head_only_commit_does_not_move_content_date(self):
        self.page.write_text(
            "<html><head><title>a</title></head><body><p>alpha</p></body></html>",
            encoding="utf-8")
        self.commit("body", "2020-05-05T12:00:00+00:00")
        self.page.write_text(
            "<html><head><title>a</title><script>ga4</script></head>"
            "<body><p>alpha</p></body></html>",
            encoding="utf-8")
        self.commit("head only", "2021-07-08T12:00:00+00:00")
        dates = urlmap.DateSource(self.repo)
        self.assertEqual(
            dates.page_lastmod("page.html", self.page, {"https://xerj.org/page": "1999-01-01"}),
            "2020-05-05")
        self.assertEqual(dates.prior_used, [])

    def test_source_updated_beats_later_html_commit(self):
        html = self.repo / "answers" / "demo.html"
        html.parent.mkdir()
        html.write_text("<html><body><p>page</p></body></html>", encoding="utf-8")
        self.commit("html", "2020-01-02T12:00:00+00:00")
        source = self.repo / "content" / "answers" / "demo.md"
        source.parent.mkdir(parents=True)
        source.write_text('updated: "2016-04-05"\n', encoding="utf-8")
        dates = urlmap.DateSource(self.repo)
        self.assertEqual(dates.page_lastmod("answers/demo.html", html, {}), "2016-04-05")

    def test_jsonld_date_survives_shallow_clone(self):
        self.page.write_text(
            '<html><head></head><body>'
            '<script type="application/ld+json">{"dateModified": "2018-02-03"}</script>'
            "<p>alpha</p></body></html>",
            encoding="utf-8")
        self.commit("jsonld", "2021-03-04T12:00:00+00:00")
        shallow = self.root / "shallow-jsonld"
        self.git("clone", "--depth", "1", self.repo.as_uri(), str(shallow))
        page = shallow / "page.html"
        dates = urlmap.DateSource(shallow)
        self.assertFalse(dates.git_ok)
        self.assertEqual(dates.page_lastmod("page.html", page, {}), "2018-02-03")
        self.assertEqual(dates.fallbacks, [])

    def test_shallow_keeps_committed_sitemap_lastmod(self):
        shallow = self.root / "shallow-prior"
        self.git("clone", "--depth", "1", self.repo.as_uri(), str(shallow))
        page = shallow / "page.html"
        stamp = dt.datetime(2022, 5, 6, 12, tzinfo=dt.timezone.utc).timestamp()
        os.utime(page, (stamp, stamp))
        loc = urlmap.deployed_url("page.html")
        sitemap = shallow / "sitemap.xml"
        sitemap.write_text(
            '<?xml version="1.0" encoding="UTF-8"?>\n'
            '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">\n'
            "  <url>\n"
            f"    <loc>{loc}</loc>\n"
            "    <lastmod>2017-11-11</lastmod>\n"
            "  </url>\n"
            "</urlset>\n",
            encoding="utf-8")
        dates = urlmap.DateSource(shallow)
        prior = dates.committed_lastmods(sitemap)
        self.assertEqual(dates.page_lastmod("page.html", page, prior), "2017-11-11")
        self.assertEqual(dates.prior_used, ["page.html"])
        self.assertEqual(dates.fallbacks, [])
        built = ET.fromstring(gen_sitemap.build(shallow, dates))
        self.assertEqual(built.findtext(".//{*}lastmod"), "2017-11-11")


if __name__ == "__main__":
    unittest.main()
