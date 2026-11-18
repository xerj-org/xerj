#!/usr/bin/env python3
"""Regression tests for the required `updated:` frontmatter field (#974).

Until #974 an article could omit `updated:`, and build_articles fell back to
the content file's git committer date (`article.updated or
context.dates.last_modified(...)`).  A squash merge rewrites that date to the
merge day, so any PR whose pages were regenerated on day N went red on
`build_articles --check` the moment it merged on day N+1 (#942 was the live
instance; #972 band-aided it).  These tests pin the fix: the field is
materialized in every content source and `article_data.load` rejects a source
without it, so article dates are author-controlled data and cannot drift with
git history.

Run: python3 scripts/seo/test_article_schema.py
"""

import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

import article_data
from article_data import FrontmatterError, load

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent.parent

# Minimal frontmatter that passes _validate as written; each test mutates
# only the `updated:` line it exercises.
FRONTMATTER = """---
title: "Schema test article"
h1: "H1 of the schema test article?"
description: "A minimal article that satisfies the frontmatter schema."
slug: "schema-test-article"
cluster: "Test cluster"
question: "What does the schema test article cover?"
intent: "how-to"
published: "2026-08-21"
updated: "2026-09-01"
author: "XERJ documentation team"
reviewer: "XERJ engineering team"
schema_type: "TechArticle"
links_out:
  - /answers/what-is-xerj/
faq:
  - q: "Question one?"
    a: "Answer one."
  - q: "Question two?"
    a: "Answer two."
  - q: "Question three?"
    a: "Answer three."
  - q: "Question four?"
    a: "Answer four."
---
Body of the schema test article.
"""


def _write(tmp: pathlib.Path, text: str) -> pathlib.Path:
    # The slug must match the filename and the parent directory selects the
    # category, so the fixture layout mirrors content/answers/.
    directory = tmp / "answers"
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / "schema-test-article.md"
    path.write_text(text, encoding="utf-8")
    return path


class UpdatedFieldTests(unittest.TestCase):
    def test_missing_updated_is_rejected(self):
        # The #974 regression: before the field became required this loaded
        # cleanly and the generator silently dated the page from git.
        text = FRONTMATTER.replace('updated: "2026-09-01"\n', "")
        with tempfile.TemporaryDirectory() as tmp:
            path = _write(pathlib.Path(tmp), text)
            with self.assertRaises(FrontmatterError) as ctx:
                load(path)
            self.assertIn("missing required frontmatter field(s): updated",
                          str(ctx.exception))

    def test_valid_updated_is_parsed(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = _write(pathlib.Path(tmp), FRONTMATTER)
            article = load(path)
        self.assertEqual(article.updated, "2026-09-01")

    def test_malformed_updated_is_rejected(self):
        text = FRONTMATTER.replace('updated: "2026-09-01"',
                                   'updated: "Sep 1st"')
        with tempfile.TemporaryDirectory() as tmp:
            path = _write(pathlib.Path(tmp), text)
            with self.assertRaises(FrontmatterError) as ctx:
                load(path)
            self.assertIn("updated must be an ISO date YYYY-MM-DD",
                          str(ctx.exception))

    def test_impossible_calendar_date_is_rejected(self):
        # Matches the YYYY-MM-DD shape but is not a real date, so only the
        # date.fromisoformat fallback catches it.
        text = FRONTMATTER.replace('updated: "2026-09-01"',
                                   'updated: "2026-09-32"')
        with tempfile.TemporaryDirectory() as tmp:
            path = _write(pathlib.Path(tmp), text)
            with self.assertRaises(FrontmatterError) as ctx:
                load(path)
            self.assertIn("updated is not a valid ISO date", str(ctx.exception))


GEO_ANSWER = ("What does the schema test article cover? It covers the GEO fields: a forty "
              "to sixty word answer that restates the question, one dated and sourced number, "
              "an intent group, the literal queries the page targets, and optional HowTo steps "
              "taken from the commands. This sentence pads the answer past forty words.")


def _geo(text: str = FRONTMATTER, **extra: str) -> str:
    lines = [f'answer: "{GEO_ANSWER}"',
             'key_fact: "The suite passes 1444 of 1454 cases."',
             'key_fact_source: "https://github.com/xerj-org/xerj"',
             'key_fact_date: "2026-10-19"',
             'intent_group: "code search for agents"',
             "queries:", '  - "local code search MCP"']
    for key, value in extra.items():
        lines = [line for line in lines if not line.startswith(key + ":")]
        if value is not None:
            lines.append(f"{key}: {value}")
    return text.replace('schema_type: "TechArticle"\n',
                        'schema_type: "TechArticle"\n' + "\n".join(lines) + "\n")


class GeoFieldTests(unittest.TestCase):
    def _load(self, text: str):
        with tempfile.TemporaryDirectory() as tmp:
            return load(_write(pathlib.Path(tmp), text))

    def _rejects(self, text: str, needle: str) -> None:
        with self.assertRaises(FrontmatterError) as ctx:
            self._load(text)
        self.assertIn(needle, str(ctx.exception))

    def test_valid_geo_fields_load_and_reach_head_meta(self):
        article = self._load(_geo())
        self.assertEqual(article.intent_group, "code search for agents")
        self.assertEqual(list(article.queries), ["local code search MCP"])
        meta = article.head_meta()
        self.assertNotIn("faq", meta)          # FAQPage stays opt-in (#1928)
        self.assertNotIn("items", meta)

    def test_short_answer_is_rejected(self):
        self._rejects(_geo(answer='"Too short to restate anything."'), "answer")

    def test_key_fact_needs_its_source_and_date(self):
        self._rejects(_geo(key_fact_date=None), "go together")

    def test_key_fact_needs_a_number(self):
        self._rejects(_geo(key_fact='"No number here."'), "number")

    def test_missing_repo_source_is_rejected(self):
        self._rejects(_geo(key_fact_source='"docs/does-not-exist.md"'), "key_fact_source")

    def test_capture_source_is_accepted(self):
        article = self._load(_geo(key_fact_source='"capture"'))
        self.assertEqual(article.key_fact_source, "capture")

class ItemListTests(unittest.TestCase):
    def test_items_become_an_item_list(self):
        import seo_head
        meta = {"items": [{"name": "ripgrep", "url": "https://github.com/BurntSushi/ripgrep",
                           "license": "MIT OR Unlicense", "install": "cargo install ripgrep"}],
                "items_name": "Best **grep** alternative"}
        (block,) = seo_head.geo_blocks("https://xerj.org/compare/x", meta)
        self.assertEqual(block["@type"], "ItemList")
        self.assertEqual(block["name"], "Best grep alternative")
        item = block["itemListElement"][0]["item"]
        self.assertEqual((item["name"], item["license"]), ("ripgrep", "MIT OR Unlicense"))

    def test_no_items_no_block(self):
        import seo_head
        self.assertEqual(seo_head.geo_blocks("https://xerj.org/compare/x", {}), [])


class FaqVisibleGateTests(unittest.TestCase):
    """Rule 46b (#1928) must fire on a FAQPage the page does not show."""

    def _issues(self, body: str) -> list[str]:
        import seo_lint
        node = {"@type": "FAQPage", "mainEntity": [{
            "@type": "Question", "name": "Does it run offline?",
            "acceptedAnswer": {"@type": "Answer", "text": "Yes, with the default embedder."}}]}
        with tempfile.TemporaryDirectory() as tmp:
            root = pathlib.Path(tmp)
            (root / "x.html").write_text(f"<html><head></head><body>{body}</body></html>")
            lint = object.__new__(seo_lint.Lint)
            lint.root, lint.issues = root, []
            lint._check_faq_visible("x.html", 1, node)
        return [issue.rule for issue in lint.issues]

    def test_visible_faq_passes(self):
        self.assertEqual(self._issues("<h3>Does it run offline?</h3>"
                                      "<p>Yes, with the default embedder.</p>"), [])

    def test_hidden_question_fails(self):
        self.assertEqual(self._issues("<p>Yes, with the default embedder.</p>"),
                         ["jsonld.faq_not_visible"])

    def test_wrong_answer_fails(self):
        self.assertEqual(self._issues("<h3>Does it run offline?</h3><p>No.</p>"),
                         ["jsonld.faq_not_visible"])


class CommittedContentTests(unittest.TestCase):
    def test_every_committed_article_carries_updated(self):
        """No source may regress to the git-date fallback (#974's root cause).

        load() already rejects a missing field, so this doubles as a check
        that the whole committed corpus still parses under the stricter
        schema.
        """
        sources = sorted(
            (REPO_ROOT / "content").glob("answers/*.md")
        ) + sorted((REPO_ROOT / "content").glob("compare/*.md"))
        self.assertGreater(len(sources), 0, "no content sources found")
        bad: list[str] = []
        for path in sources:
            try:
                article = article_data.load(path)
            except FrontmatterError as exc:
                bad.append(str(exc))
                continue
            if not article.updated:
                bad.append(f"{path}: updated is missing")
        self.assertEqual(bad, [])


class StructuredDataOptInTests(unittest.TestCase):
    """The opt-in FAQPage/HowTo/SoftwareApplication JSON-LD (README trap 6)."""

    OPT_IN = ('structured_data: [faq, howto, software]\n'
              'og_image: "og/answers-schema-test-article.png"\n'
              'og_image_alt: "Cover for the schema test article."\n'
              'commands:\n'
              '  - cmd: "xerj autoindex ./data"\n'
              '    note: "Index the folder"\n'
              '  - cmd: "xerj search \'query\'"\n'
              '    note: "Search it"\n')

    def _load(self, extra: str):
        text = FRONTMATTER.replace("faq:\n", extra + "faq:\n", 1)
        with tempfile.TemporaryDirectory() as tmp:
            return load(_write(pathlib.Path(tmp), text))

    def test_default_is_off(self):
        with tempfile.TemporaryDirectory() as tmp:
            article = load(_write(pathlib.Path(tmp), FRONTMATTER))
        self.assertEqual(article.structured_data, ())
        self.assertNotIn("structured_data", article.head_meta())

    def test_opt_in_emits_three_blocks(self):
        import seo_head
        article = self._load(self.OPT_IN)
        meta = article.head_meta()
        self.assertEqual(meta["og_image"], "og/answers-schema-test-article.png")
        blocks = seo_head.extra_structured_data("https://xerj.org/x", "d", meta)
        self.assertEqual([b["@type"] for b in blocks],
                         ["FAQPage", "HowTo", "SoftwareApplication"])
        self.assertEqual(len(blocks[0]["mainEntity"]), 4)
        self.assertEqual(blocks[1]["step"][1]["text"], "xerj search 'query'")
        self.assertNotIn("aggregateRating", blocks[2])

    def test_unknown_kind_is_rejected(self):
        with self.assertRaises(FrontmatterError) as ctx:
            self._load("structured_data: [faq, review]\n")
        self.assertIn("must be one of", str(ctx.exception))

    def test_howto_needs_commands(self):
        with self.assertRaises(FrontmatterError) as ctx:
            self._load("structured_data: [howto]\n")
        self.assertIn("howto needs at least two commands", str(ctx.exception))

    def test_og_image_must_match_slug(self):
        with self.assertRaises(FrontmatterError):
            self._load('og_image: "og/answers-other.png"\nog_image_alt: "x"\n')
        with self.assertRaises(FrontmatterError):
            self._load('og_image: "og/answers-schema-test-article.png"\n')


if __name__ == "__main__":
    unittest.main()
