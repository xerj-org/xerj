#!/usr/bin/env python3
"""Date-source regressions. Run: python3 scripts/seo/test_urlmap.py."""

import datetime as dt
import os
import pathlib
import subprocess
import tempfile
import unittest
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


if __name__ == "__main__":
    unittest.main()
