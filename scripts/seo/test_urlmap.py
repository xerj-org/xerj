#!/usr/bin/env python3
"""Date-source regressions. Run: python3 scripts/seo/test_urlmap.py."""

import datetime as dt
import os
import pathlib
import subprocess
import tempfile
import unittest
import unittest.mock
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

    def count_git(self):
        calls = []
        real = subprocess.run

        def run(cmd, *args, **kwargs):
            if cmd and cmd[0] == "git":
                calls.append(cmd)
            return real(cmd, *args, **kwargs)

        patcher = unittest.mock.patch.object(urlmap.subprocess, "run", run)
        patcher.start()
        self.addCleanup(patcher.stop)
        return calls

    def test_many_pages_cost_one_git_walk(self):
        """#3226: dates for N pages are one `git log`, not N."""
        pages = []
        for i in range(30):
            page = self.repo / "docs" / f"p{i}.html"
            page.parent.mkdir(exist_ok=True)
            page.write_text(f"page {i}", encoding="utf-8")
            pages.append(page)
            self.commit(f"p{i}", f"2022-01-{i % 28 + 1:02d}T12:00:00+00:00")
        pages[3].write_text("page 3, edited", encoding="utf-8")
        self.commit("edit p3", "2023-06-07T12:00:00+00:00")
        expected = {}
        reference = urlmap.DateSource(self.repo)
        for page in pages + [self.page]:
            expected[page] = (reference._git_log_path(page),
                              reference._git_log_path(page, first=True))
        dates = urlmap.DateSource(self.repo)
        calls = self.count_git()
        for page in pages + [self.page]:
            self.assertEqual((dates.last_modified(page), dates.first_published(page)),
                             expected[page], page)
        self.assertEqual(len(calls), 1, calls)
        self.assertEqual(dates.last_modified(pages[3]), "2023-06-07")
        self.assertEqual(dates.first_published(pages[3]), "2022-01-04")
        self.assertEqual(dates.fallbacks, [])

    def test_relative_path_reads_from_the_repository_root(self):
        dates = urlmap.DateSource(self.repo)
        self.assertEqual(dates.last_modified(pathlib.Path("page.html")), "2020-01-02")

    def test_conflict_merge_keeps_its_own_date(self):
        """A merge that changed the page against both parents is its last change."""
        repo = str(self.repo)
        base = self.git("-C", repo, "rev-parse", "--abbrev-ref", "HEAD")
        self.git("-C", repo, "checkout", "-b", "side")
        self.page.write_text("side version", encoding="utf-8")
        self.commit("side", "2020-02-01T12:00:00+00:00")
        self.git("-C", repo, "checkout", base)
        self.page.write_text("main version", encoding="utf-8")
        self.commit("main", "2020-03-01T12:00:00+00:00")
        env = {**os.environ, "GIT_AUTHOR_DATE": "2020-04-01T12:00:00+00:00",
               "GIT_COMMITTER_DATE": "2020-04-01T12:00:00+00:00"}
        subprocess.run(["git", "-C", repo, "merge", "side"], capture_output=True, env=env)
        self.page.write_text("resolved version", encoding="utf-8")
        self.git("-C", repo, "add", ".")
        self.git("-C", repo, "commit", "--no-edit", env=env)
        dates = urlmap.DateSource(self.repo)
        self.assertEqual(dates._git_log_path(self.page), "2020-04-01")
        self.assertEqual(dates.last_modified(self.page), "2020-04-01")
        self.assertEqual(dates.first_published(self.page),
                         dates._git_log_path(self.page, first=True))

    def test_directory_falls_back_to_a_per_path_log(self):
        dates = urlmap.DateSource(self.repo)
        self.assertEqual(dates.last_modified(self.repo), "2021-03-04")
        self.assertEqual(dates.fallbacks, [])


if __name__ == "__main__":
    unittest.main()
