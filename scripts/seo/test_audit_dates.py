#!/usr/bin/env python3
"""`Audit.last_change_date` resolves page paths from the repository root.

Git runs at the repo root, so a page path relative to `landing/` matched nothing
and every page reported no date — which silently disabled the audit's
`sitemap.stale_lastmod` comparison instead of failing loudly.

Run: python3 scripts/seo/test_audit_dates.py
"""

import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import audit  # noqa: E402
import urlmap  # noqa: E402

PAGE = "index.html"


class LastChangeDateTests(unittest.TestCase):
    def setUp(self):
        if not urlmap.DateSource(urlmap.repo_root()).git_ok:
            self.skipTest("git history is unavailable in this checkout")
        self.audit = audit.Audit(urlmap.landing_dir())

    def test_a_committed_page_has_a_git_date(self):
        self.assertRegex(self.audit.last_change_date(PAGE) or "",
                         r"^\d{4}-\d{2}-\d{2}$")

    def test_the_date_is_the_one_the_sitemap_generator_writes(self):
        dates = urlmap.DateSource(urlmap.repo_root())
        self.assertEqual(
            self.audit.last_change_date(PAGE),
            dates.git_date(pathlib.Path("landing") / PAGE),
        )

    def test_a_page_that_is_not_in_the_tree_has_no_date(self):
        self.assertIsNone(self.audit.last_change_date("no-such-page.html"))


if __name__ == "__main__":
    unittest.main(verbosity=2)
