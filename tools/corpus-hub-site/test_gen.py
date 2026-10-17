#!/usr/bin/env python3
"""Regression tests for gen.py (the Corpus Hub site generator).

Stdlib only, no network, deterministic — same constraints as gen.py. Run:

    python3 tools/corpus-hub-site/test_gen.py

Covers issue #1279: a graded backlog file whose "graded" value is a string
(a timestamp note, not a slug->suite map) must not crash the whole site
build. One malformed file froze every corpus discovery page in the deploy.

Also covers Corpus Hub v2, plan section 9
(docs/CORPUS_HUB_V2_IMPLEMENTATION.md): the copy prompt and the "Use it" block
are the one line plus the `--def` form, nothing on the site clones, adds or
indexes a corpus, and the catalogue link is on every page.
"""
import contextlib
import io
import json
import pathlib
import sys
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import gen  # noqa: E402


class ParseGradedSuitesTest(unittest.TestCase):
    def _run_against(self, files):
        """Run parse_graded_suites against a throwaway backlog dir."""
        with tempfile.TemporaryDirectory() as d:
            hub = pathlib.Path(d)
            (hub / "backlog").mkdir()
            for name, payload in files.items():
                (hub / "backlog" / name).write_text(json.dumps(payload))
            orig = gen.HUB
            gen.HUB = hub
            try:
                return gen.parse_graded_suites()
            finally:
                gen.HUB = orig

    def test_string_graded_value_is_skipped_not_fatal(self):
        # issue #1279: "graded" is a timestamp string, not a dict. Before the
        # fix this raised AttributeError: 'str' object has no attribute 'items'
        # and aborted the entire deploy.
        out = self._run_against({
            "g7-otel-proto-graded.json": {
                "suite": "g7-otel-proto",
                "graded": "2026-10-08 (re-graded same day; run 2 PASS 5/5)",
                "verdict": "PASS",
            },
        })
        self.assertEqual(out, {})

    def test_well_formed_dict_suite_still_parses(self):
        out = self._run_against({
            "g7-good-graded.json": {
                "graded": {
                    "mycorpus": {
                        "verdict": "PASS",
                        "queries": [
                            {"q": "a", "verdict": "relevant"},
                            {"q": "b", "verdict": "relevant"},
                        ],
                    }
                }
            },
        })
        self.assertIn("mycorpus", out)
        self.assertEqual(out["mycorpus"]["median"], 5.0)

    def test_malformed_file_does_not_hide_good_siblings(self):
        # The real-world failure mode: the whole build must survive one bad
        # file and still emit every other corpus' suite.
        out = self._run_against({
            "g7-bad-graded.json": {"graded": "just a note"},
            "g7-ok-graded.json": {
                "corpora": {
                    "ok": {"verdict": "PASS",
                           "queries": [{"q": "x", "verdict": "relevant"}]}
                }
            },
        })
        self.assertIn("ok", out)

    def test_list_graded_value_is_also_tolerated(self):
        out = self._run_against({
            "g7-list-graded.json": {"graded": ["not", "a", "map"]},
        })
        self.assertEqual(out, {})


# A live, lane-A corpus: the only kind that has a hub pack to ask.
LANE_A = {
    "slug": "lz4", "lane": "A", "cat": "NET", "status": "live", "refresh": "pinned",
    "usecase": "an agent needing fast block compression queries this for the block format",
    "repos": [{
        "repo": "lz4/lz4", "url": "https://github.com/lz4/lz4", "sha": "a" * 40,
        "bytes": 1000, "files": 10,
        "review": {"spdx": "BSD-2-Clause", "use": "adapt-with-attribution", "note": "ok"},
    }],
    "added": "2026-10-01T00:00:00Z", "kind": "reference corpus", "consumed": "",
    "tags": {"region": "global", "topics": ["compression"]}, "merged_into": "",
}

# What the v1 flow printed. None of it may come back (plan section 9: the pages
# become the one line plus `--def`; the pack is fetched, not cloned or indexed).
V1_FLOW = (
    "corpus add", "corpus index", "curl -fsSLO", "xerj code ", "xerj_code_search",
    "--from", "/tools/xerj-code/hub/",
)


class HubV2CopyTest(unittest.TestCase):
    def assertNoV1Flow(self, text, what):
        for banned in V1_FLOW:
            self.assertNotIn(banned, text, f"{what} still prints the v1 flow: {banned!r}")

    def test_corpus_prompt_is_the_one_line_plus_def(self):
        p = gen.install_prompt(LANE_A)
        self.assertIn('xerj "<your question>" --corpus lz4', p)
        self.assertIn("xerj ask --def <SYMBOL> --corpus lz4", p)
        # the bare form needs a space in the question; one word needs `ask`
        self.assertIn('xerj ask "<word>" --corpus lz4', p)
        self.assertNoV1Flow(p, "the per-corpus copy prompt")

    def test_corpus_prompt_names_success_and_the_node_rule(self):
        p = gen.install_prompt(LANE_A)
        # step 1 finds an older binary before step 2 fails on it
        self.assertIn("xerj ask --help", p)
        # the three success lines, in the renderer's shape
        self.assertIn("lz4  pin <date> (<N> days old)  <licence>  <file>:<line>", p)
        self.assertIn("answered from: local pack", p)
        # stop only the node you started
        self.assertIn("xerj hub stop", p)
        self.assertIn("Never pkill or killall xerj", p)
        # the no-shell route and the catalogue
        self.assertIn('xerj_hub_ask with {"question": "<your question>", "corpus": "lz4"}', p)
        self.assertIn(gen.CATALOGUE_URL, p)

    def test_prompts_say_a_hub_tool_never_downloads(self):
        for p in (gen.install_prompt(LANE_A), gen.category_prompt("NET", "Networking", [LANE_A])):
            self.assertIn("never download", p)
            self.assertIn("xerj hub pull <slug>", p)
            self.assertNotIn("hosted hub", p)

    def test_domain_prompt_needs_no_corpus_name(self):
        p = gen.category_prompt("NET", "Networking", [LANE_A])
        self.assertIn('2. Ask: xerj "<your question>"\n', p)
        self.assertIn("xerj ask --def <SYMBOL>\n", p)
        self.assertIn('xerj ask "<word>"\n', p)
        self.assertIn("lz4 (", p)  # the domain's corpora are still listed for context
        self.assertIn("Exit 4", p)  # ambiguous: the candidates print, re-run with --corpus
        self.assertNoV1Flow(p, "the per-domain copy prompt")
        self.assertIn(gen.CATALOGUE_URL, p)

    def test_no_em_or_en_dash_in_generated_copy(self):
        # user directive 2026-10-06: plain short sentences, no em/en dashes
        body = gen.detail_body(LANE_A, {}, {}, {}, {})
        for name, text in (("corpus prompt", gen.install_prompt(LANE_A)),
                           ("domain prompt", gen.category_prompt("NET", "Networking", [LANE_A])),
                           ("Use it block", body)):
            self.assertNotIn("\u2014", text, name)
            self.assertNotIn("\u2013", text, name)

    def test_use_it_block_is_the_one_line_plus_def(self):
        body = gen.detail_body(LANE_A, {}, {}, {}, {})
        self.assertIn('<code>xerj "your question" --corpus lz4</code>', body)
        self.assertIn("<code>xerj ask --def SYMBOL --corpus lz4</code>", body)
        self.assertIn("<code>xerj_hub_ask</code>", body)
        self.assertIn('href="../llms.txt"', body)  # the catalogue, from a corpus page
        self.assertNoV1Flow(body, "the Use-it block")

    def test_entry_without_a_pack_prints_no_command(self):
        # Lane B (curated pack) and non-live rows have no hub pack: a command
        # for them could only fail, so none is printed and there is no prompt.
        for patch in ({"lane": "B"}, {"status": "candidate"}):
            c = dict(LANE_A, **patch)
            body = gen.detail_body(c, {}, {}, {}, {})
            self.assertIn("no hub pack", body)
            self.assertNotIn("--corpus lz4", body)
            self.assertNotIn('<button class="copy"', body)

    def test_withdrawn_entry_still_points_at_its_replacement(self):
        c = dict(LANE_A, merged_into="lz4-next")
        body = gen.detail_body(c, {}, {}, {}, {})
        self.assertIn("Withdrawn", body)
        self.assertIn("lz4-next.html", body)

    def test_page_chrome_links_the_catalogue(self):
        self.assertIn('<a href="llms.txt" title="The catalogue', gen.page("t", "d", "<main></main>"))
        self.assertIn('<a href="../llms.txt" title="The catalogue',
                      gen.page("t", "d", "<main></main>", rel="../"))
        self.assertIn(">CATALOGUE</a>", gen.page("t", "d", "<main></main>"))


class GeneratedSiteTest(unittest.TestCase):
    """Run the real generator against the real registry into a temp dir."""

    @classmethod
    def setUpClass(cls):
        cls._tmp = tempfile.TemporaryDirectory()
        cls.out = pathlib.Path(cls._tmp.name)
        orig = gen.OUT
        gen.OUT = cls.out
        try:
            with contextlib.redirect_stdout(io.StringIO()):  # main() prints one summary line
                gen.main()
        finally:
            gen.OUT = orig

    @classmethod
    def tearDownClass(cls):
        cls._tmp.cleanup()

    def test_every_page_links_the_catalogue(self):
        pages = sorted(self.out.rglob("*.html"))
        self.assertGreater(len(pages), 100)
        for page in pages:
            rel = "../" * (len(page.relative_to(self.out).parts) - 1)
            text = page.read_text(encoding="utf-8")
            self.assertIn(f'<a href="{rel}llms.txt" title="The catalogue', text, str(page))

    def test_no_page_prints_the_v1_flow(self):
        for page in sorted(self.out.rglob("*.html")):
            text = page.read_text(encoding="utf-8")
            for banned in ("curl -fsSLO", "xerj corpus add --from", "xerj corpus index",
                           "xerj_code_search"):
                self.assertNotIn(banned, text, f"{page.name}: {banned}")

    def test_hub_llms_txt_is_the_one_line_flow(self):
        text = (self.out / "llms.txt").read_text(encoding="utf-8")
        for needed in ('xerj "your question"', 'xerj ask "word"', "xerj ask --def <SYMBOL>",
                       "xerj hub pull <slug>", "xerj hub stop", "xerj_hub_ask",
                       "pulled <slug> <pin8> <bytes> \u2192 <n> indices, <defs> definitions"):
            self.assertIn(needed, text)
        for banned in ("curl -fsSLO", "xerj corpus add", "xerj corpus index",
                       "xerj code <slug>", "xerj_code_search"):
            self.assertNotIn(banned, text)

    def test_nothing_claims_a_hosted_answer_or_a_question_api(self):
        # The CLI has no hosted client (`run_ask(.., None)`), and hub.xerj.org
        # serves no question API (plan section 12: hosted is out of scope), so no
        # page, prompt or the hub llms.txt may print a hosted answer or an
        # /api/v1/ question route.
        texts = [("llms.txt", (self.out / "llms.txt").read_text(encoding="utf-8"))]
        texts += [(p.name, p.read_text(encoding="utf-8")) for p in sorted(self.out.rglob("*.html"))]
        for name, text in texts:
            for banned in ("hosted hub", "api/v1/search", "api/v1/code", "api/v1/def"):
                self.assertNotIn(banned, text, f"{name}: {banned}")

    def test_hub_llms_txt_names_every_exit_code_and_the_mcp_limits(self):
        text = (self.out / "llms.txt").read_text(encoding="utf-8")
        for code in ("Exit 2", "Exit 3", "Exit 4", "Exit 5", "(exit 1)"):
            self.assertIn(code, text)
        self.assertIn("xerj_hub_find", text)
        self.assertIn("never downloads", text)
        # per-client MCP registration: Claude Code line, both root keys
        self.assertIn("claude mcp add --scope user xerj", text)
        self.assertIn("`mcpServers`", text)
        self.assertIn("root key `servers`", text)
        # local corpora: the prefix the query must repeat (#1580)
        self.assertIn("xerj search --prefix <name>", text)

    def test_hub_llms_txt_first_screen_has_no_long_line(self):
        # llms.txt study rule 7: no line over about 300 characters above the
        # first command. The corpus list below "Live corpora" is data, not prose.
        text = (self.out / "llms.txt").read_text(encoding="utf-8")
        head = text.split("## Live corpora")[0]
        self.assertTrue(all(len(line) <= 300 for line in head.splitlines()))


if __name__ == "__main__":
    unittest.main()
