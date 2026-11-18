#!/usr/bin/env python3
"""The audit dates a page the same way the sitemap generator does.

`landing/sitemap.xml` is a deploy artifact and is not in the tree, so the audit
builds the document it would ship and compares each `<lastmod>` against a date
from the same `urlmap.DateSource`. If the two ever read different dates (a
`git log --format=%cs` here, a UTC-normalised content date there) a freshly
generated sitemap reports itself stale.

Run: python3 scripts/seo/test_audit_dates.py
"""

import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import audit  # noqa: E402
import urlmap  # noqa: E402

PAGE = "index.html"


class SitemapDateTests(unittest.TestCase):
    def setUp(self):
        self.landing = urlmap.landing_dir()
        self.audit = audit.Audit(self.landing)

    def test_a_published_page_has_a_date(self):
        self.assertRegex(self.audit.last_change_date(PAGE) or "",
                         r"^\d{4}-\d{2}-\d{2}$")

    def test_a_page_that_is_not_in_the_tree_has_no_date(self):
        self.assertIsNone(self.audit.last_change_date("no-such-page.html"))

    def test_the_shipped_sitemap_never_reports_itself_stale(self):
        self.audit.load()
        self.audit.run()
        stale = [f for f in self.audit.findings if f.check == "sitemap.stale_lastmod"]
        self.assertEqual([(f.path, f.message) for f in stale], [])


if __name__ == "__main__":
    unittest.main(verbosity=2)
