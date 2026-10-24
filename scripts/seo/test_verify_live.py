#!/usr/bin/env python3
"""Offline canonical checks. Run: python3 scripts/seo/test_verify_live.py."""

import contextlib
import io
import json
import pathlib
import tempfile
import unittest
from unittest.mock import patch

import verify_live


class CanonicalTests(unittest.TestCase):
    @staticmethod
    def page(canonical):
        return f'<link rel="canonical" href="{canonical}">'.encode()

    def check(self, url, canonical):
        with patch.object(verify_live, "_fetch",
                          return_value=(200, self.page(canonical), None)):
            return verify_live.check_one(url, None, 1)

    def test_matching_canonical_urls_pass(self):
        for url in ["https://xerj.org/", "https://xerj.org/playground",
                    "https://xerj.org/playground/"]:
            with self.subTest(url=url):
                result = self.check(url, url)
                self.assertTrue(result.ok, result.as_dict())
                self.assertEqual(result.canonical, url)

    def test_trailing_slash_mismatches_fail_rule_22(self):
        # These paths can serve different files; urlmap documents this pair.
        for url, canonical in [
            ("https://xerj.org/playground", "https://xerj.org/playground/"),
            ("https://xerj.org/playground/", "https://xerj.org/playground"),
        ]:
            with self.subTest(url=url):
                result = self.check(url, canonical)
                self.assertFalse(result.ok, result.as_dict())
                self.assertEqual(result.failures, [
                    f"canonical {canonical} != requested {url} (rule 22)"])

    def test_other_canonical_mismatches_fail_rule_22(self):
        url = "https://xerj.org/playground"
        for canonical in ["https://xerj.org/product",
                          "https://example.invalid/playground"]:
            with self.subTest(canonical=canonical):
                result = self.check(url, canonical)
                self.assertFalse(result.ok, result.as_dict())
                self.assertEqual(result.failures, [
                    f"canonical {canonical} != requested {url} (rule 22)"])

    def run_main(self, *args):
        home = "https://xerj.org/"
        url = "https://xerj.org/playground"
        responses = {
            home: (200, self.page(home), None),
            url: (200, self.page(url + "/"), None),
            "https://xerj.org" + verify_live.NOT_FOUND_PROBE: (404, b"missing", None),
        }
        with tempfile.TemporaryDirectory() as tmp:
            sitemap = pathlib.Path(tmp) / "sitemap.xml"
            sitemap.write_text(
                '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">'
                f"<url><loc>{url}</loc></url></urlset>", encoding="utf-8")
            output = io.StringIO()
            with patch.object(verify_live, "_fetch",
                              side_effect=lambda u, timeout: responses[u]), \
                    contextlib.redirect_stdout(output):
                status = verify_live.main([
                    "--sitemap", str(sitemap), "--jobs", "1", *args])
            return status, output.getvalue()

    def test_json_report_fails_for_trailing_slash_mismatch(self):
        status, output = self.run_main("--json")
        self.assertEqual(status, 1, output)
        report = json.loads(output)
        self.assertEqual(report["failed"], 1)
        self.assertEqual(report["checked"], 2)
        self.assertTrue(report["not_found_probe"]["ok"])
        result = next(r for r in report["results"]
                      if r["url"] == "https://xerj.org/playground")
        self.assertFalse(result["ok"])
        self.assertEqual(result["failures"], [
            "canonical https://xerj.org/playground/ != requested "
            "https://xerj.org/playground (rule 22)"])

    def test_text_report_fails_for_trailing_slash_mismatch(self):
        status, output = self.run_main()
        self.assertEqual(status, 1, output)
        self.assertIn("FAIL https://xerj.org/playground\n", output)
        self.assertIn(
            "canonical https://xerj.org/playground/ != requested "
            "https://xerj.org/playground (rule 22)", output)
        self.assertIn("1 failure(s)", output)


if __name__ == "__main__":
    unittest.main()
