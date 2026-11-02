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
import collections
import contextlib
import csv
import html
import io
import json
import os
import pathlib
import re
import struct
import sys
import urllib.parse
import tempfile
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import gen  # noqa: E402
import contributors  # noqa: E402
import gh_stars  # noqa: E402
import repo_pages  # noqa: E402
import search_index  # noqa: E402


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

    def test_corpus_prompt_fetches_with_hub_pull_first(self):
        # section 12.2: the page's fetch step is the one-line hub pull, before
        # the first question, and it says what a failed verification looks like
        p = gen.install_prompt(LANE_A)
        self.assertIn("2. Fetch the pack once: xerj hub pull lz4\n", p)
        self.assertLess(p.index("xerj hub pull lz4"), p.index('3. Ask: xerj "<your question>" --corpus lz4'))
        self.assertIn("Exit 3", p)

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
        # section 12.2: the fetch is the one-line hub pull, listed first
        self.assertIn("<code>xerj hub pull lz4</code><br>", body)
        self.assertLess(body.index("<code>xerj hub pull lz4</code>"),
                        body.index('<code>xerj "your question" --corpus lz4</code>'))
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
            self.assertNotIn("hub pull lz4", body)
            self.assertNotIn('<button class="copy"', body)

    def test_live_corpus_without_a_published_pack_prints_no_command(self):
        # A live lane-A manifest whose pack has never been published must not
        # get pull/ask commands: that would advertise a pack that does not exist.
        saved = gen._PACKED
        try:
            gen._PACKED = {"some-other-corpus"}
            body = gen.detail_body(dict(LANE_A), {}, {}, {}, {})
            self.assertIn("no hub pack", body)
            self.assertNotIn("xerj hub pull lz4", body)
            self.assertNotIn("--corpus lz4", body)
            gen._PACKED = {"lz4"}
            self.assertIn("xerj hub pull lz4", gen.detail_body(dict(LANE_A), {}, {}, {}, {}))
        finally:
            gen._PACKED = saved

    def test_withdrawn_entry_still_points_at_its_replacement(self):
        c = dict(LANE_A, merged_into="lz4-next")
        body = gen.detail_body(c, {}, {}, {}, {})
        self.assertIn("Withdrawn", body)
        self.assertIn('href="lz4-next"', body)

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
            if page.name == "404.html":
                # The 404 page is served at any depth, so its links are absolute.
                rel = "/"
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
        # The usage / traffic / most-used / privacy panels (usage.py) describe
        # what the hub MEASURES, including calls to the not-yet-deployed hosted
        # API; they promise no answer. Every other surface must not mention it.
        panels = re.compile(
            r'<section class="detail (?:usage|traffic|mostused)".*?</section>'
            r'|<section class="detail" id="privacy">.*?</section>'
            r"|<script\b.*?</script>", re.S)
        texts = [(name, panels.sub("", text)) for name, text in texts]
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

    def test_listing_names_the_upstream_repo_not_just_the_slug(self):
        # Issue #2179: fabric / candle / webgl share a name with a different
        # project. The catalogue line, the corpus page and the search index
        # have to say which repo the slug actually is.
        text = (self.out / "llms.txt").read_text(encoding="utf-8")

        def line(slug):
            return next(l for l in text.splitlines() if l.startswith(f"- [{slug}]("))

        self.assertIn("danielmiessler/fabric", line("fabric"))
        self.assertIn("prompt", line("fabric").lower())
        self.assertIn("huggingface/candle", line("candle"))
        self.assertIn("KhronosGroup/WebGL", line("webgl"))
        page = (self.out / "corpus" / "fabric.html").read_text(encoding="utf-8")
        self.assertIn(">danielmiessler/fabric</a>", page)
        self.assertIn('class="crepo">danielmiessler/fabric</p>', page)
        idx = json.loads((self.out / "search" / "index.json").read_text(encoding="utf-8"))
        more = json.loads((self.out / "search" / "more.json").read_text(encoding="utf-8"))
        i = next(i for i, r in enumerate(idx["c"]) if "danielmiessler/fabric" in (r[1] or ""))
        self.assertEqual(idx["c"][i][0], "fabric")
        self.assertIn("prompt", more["c"][i][2].lower())
        js = (self.out / "assets" / "search.js").read_text(encoding="utf-8")
        self.assertIn("not ", js)
        self.assertIn("projectStem", js)

    def test_hub_llms_txt_first_screen_has_no_long_line(self):
        # llms.txt study rule 7: no line over about 300 characters above the
        # first command. The corpus list below "Live corpora" is data, not prose.
        text = (self.out / "llms.txt").read_text(encoding="utf-8")
        head = text.split("## Live corpora")[0]
        self.assertTrue(all(len(line) <= 300 for line in head.splitlines()))

class RepoPagesTest(unittest.TestCase):
    """Titles, descriptions and the prompt for repo pages (repo_pages.py)."""

    def test_title_keeps_the_name_and_fits(self):
        t = repo_pages.title("JacobJoergensen/laravel-paper", "A flat-file Eloquent driver for modern Laravel")
        self.assertEqual(t, "JacobJoergensen/laravel-paper: A flat-file Eloquent driver")
        self.assertEqual(repo_pages.title("tomasuribe/switch-frame-tap", ""), "tomasuribe/switch-frame-tap | XERJ Hub")
        t = repo_pages.title("humanlayer/12-factor-agents", "Reliability and design principles of production LLM agents")
        self.assertEqual(t, "humanlayer/12-factor-agents: Reliability and design principles")

    def test_title_never_ends_on_a_dangling_word(self):
        d = repo_pages.dangling
        for t in ("x: What are the principles we can", "x: Its own or another agent's | XERJ Hub",
                  "x: FAISS-based semantic search over any", "x: A driver for the", "x: Tools across", "x: Built with LangGraph and"):
            self.assertTrue(d(t), t)
        self.assertFalse(d("x: A flat-file Eloquent driver | XERJ Hub"))
        fb = repo_pages.fallback_titles("C++", True)
        cases = {
            ("humanlayer/12-factor-agents", "What are the principles we can use to build LLM-powered software that "
             "is actually good enough to put in the hands of production customers?"): "humanlayer/12-factor-agents source and stats | XERJ Hub",
            ("TomasUribe/switch-frame-tap", "Play your Nintendo Switch or Switch Lite on your PC: stream games at up "
             "to 1080p60 with sound over USB"): "TomasUribe/switch-frame-tap: C++ repo corpus for AI agents",
            ("jovempedr0/faiss_index", "FAISS-based semantic search over any folder of documents"):
                "jovempedr0/faiss_index: FAISS-based semantic search | XERJ Hub",
            ("openbsd/www", "Read-only git conversion of OpenBSD's official CVS www repository"):
                "openbsd/www: Read-only git conversion | XERJ Hub",
        }
        for (name, desc), want in cases.items():
            got = repo_pages.title(name, desc, fallbacks=fb if "switch" in name else repo_pages.fallback_titles("", True))
            self.assertEqual(got, want)
            self.assertLessEqual(len(got), 62)
            self.assertFalse(repo_pages.dangling(got), got)
        long = repo_pages.title("kalviumcommunity/S116-0826-Decrypters-Full-Stack-With-Flutter-Dart-Firebase", "")
        self.assertEqual(long, "kalviumcommunity/S116-0826-Decrypters-Full-Stack-With-Flutter")

    def test_clip_cuts_at_a_sentence_or_word_and_drops_dashes(self):
        self.assertEqual(repo_pages.clip("One two three four five", 13), "One two three")
        self.assertEqual(repo_pages.clip("One two three of four", 17), "One two three")
        self.assertEqual(repo_pages.clean("Fast \u2014 small \u2013 safe :rocket: \U0001F680"), "Fast: small, safe")

    def test_usecase_drops_the_agent_preamble(self):
        self.assertEqual(repo_pages.usecase_text("an agent queries this for the exact semantic conventions"),
                         "Exact semantic conventions")
        self.assertTrue(repo_pages.usecase_text("How sled/fjall/redb do compaction").startswith("How sled"))

    def test_description_fits_and_drops_facts_first(self):
        d = repo_pages.description("x: " + "word " * 25, ["1,234 GitHub stars", "Rust", "MIT"], ["Copy the prompt to search it with AI."])
        self.assertLessEqual(len(d), 160)
        self.assertTrue(d.endswith("Copy the prompt to search it with AI."))

    def test_clone_prompt_pins_the_commit(self):
        p = repo_pages.clone_prompt("a/b", "https://github.com/a/b", "f" * 40, "https://hub.xerj.org/corpus/a--b", "MIT", "")
        self.assertIn("git clone https://github.com/a/b b && git -C b checkout " + "f" * 40, p)
        self.assertIn("xerj autoindex ./b --url http://127.0.0.1:9200 --prefix b", p)
        self.assertIn("xerj autoindex map", p)

    def test_weak_threshold_and_guides(self):
        def f(stars, commits, files, slug="a--b", topics=(), langs=(), shelf=""):
            st = {"commits": commits, "languages": list(langs)} if commits is not None else {}
            return dict(slug=slug, st=st, stars=stars, tree={"files": files, "bytes": 1000} if files else None,
                        topics=list(topics), langs=[l for l, _ in langs], short="b", repo="a/b", shelf=shelf,
                        sha="f" * 40, spdx="MIT", w={"reviewed": True})
        self.assertIn("no GitHub facts", repo_pages.weak(f(None, None, None)))
        self.assertIn("commits", repo_pages.weak(f(2, 5, 500)))
        self.assertIn("files", repo_pages.weak(f(2, 500, 10)))
        self.assertEqual(repo_pages.weak(f(5, 1, 1)), "")
        self.assertEqual(repo_pages.weak(f(0, 50, 50)), "")
        self.assertEqual(repo_pages.weak(f(0, 1, 1), seen={"a--b"}), "")
        g = repo_pages.guides(f(9, 9, 3000, topics=["mcp"], langs=[("PHP", 90.0)]))
        urls = [u for u, _, _ in g]
        self.assertEqual(len(g), 4)
        self.assertTrue(all(u.startswith(("https://xerj.org/answers/", "https://xerj.org/compare/")) for u in urls))
        self.assertIn("https://xerj.org/answers/code-search-mcp-for-claude-code", urls)
        self.assertEqual(g[0][2], "b is tagged mcp on GitHub.")
        for u, label in repo_pages.GUIDE.values():
            self.assertNotIn("\u2014", label)
        lic = repo_pages.licence_text(dict(f(9, 9, 9), st={"licence": "Apache-2.0"}), html.escape, "2026-10-29")
        self.assertIn("but the LICENSE file", lic)
        self.assertIn("2026-10-29", lic)

    def test_candidates_reads_author_manifests(self):
        with tempfile.TemporaryDirectory() as d:
            (pathlib.Path(d) / "candidates" / "authors").mkdir(parents=True)
            (pathlib.Path(d) / "candidates" / "authors" / "a--b.json").write_text(
                json.dumps({"repos": [{"url": "https://github.com/a/b", "sha": "1" * 40}]}))
            people = {"a": {"corpora": {"a--b": "author corpus"}, "work": {"x": {"corpora": ["a--b"]}}}}
            c = contributors.candidates(people, set(), d)
            self.assertEqual(c["a--b"]["repos"][0]["sha"], "1" * 40)


class ContributorVolumeTest(unittest.TestCase):
    """Contributor pages aggregate on login across every corpus (PR #1782 follow-up)."""

    def _hub(self, d):
        hub = pathlib.Path(d)
        (hub / "candidates" / "authors").mkdir(parents=True)
        rows = [
            {"corpus": "ann--one", "repo": "Ann/one", "repo_url": "https://github.com/Ann/one", "repo_stars": 10,
             "contributor": {"login": "Ann", "name": "Ann A"}, "consent": {"at": "2026-10-09T10:00Z"},
             "contribution": {"commit": "a" * 40, "commit_url": "u", "files": 3, "lines": 100},
             "licence": {"spdx": "MIT", "reviewed": False}},
            {"corpus": "bob--two", "repo": "bob/two", "repo_url": "https://github.com/bob/two", "repo_stars": 5,
             "contributor": {"login": "ann", "name": ""}, "consent": {"at": "2026-10-08T10:00Z"},
             "contribution": {"commit": "b" * 40, "commit_url": "u", "files": 2, "lines": 50},
             "licence": {"spdx": "MIT", "reviewed": False}},
        ]
        (hub / "candidates" / "authors" / "CONTRIBUTORS.jsonl").write_text("\n".join(json.dumps(r) for r in rows) + "\n")
        (hub / "candidates" / "CONTRIBUTORS-2026-10.md").write_text(
            "## themed (2)\n\n| repo | author | commit | licence | permission |\n|---|---|---|---|---|\n"
            "| [Ann/one](https://github.com/Ann/one) | @ANN | [aaaaaaaaaa](u) same commit again | MIT | 2026-10-05 |\n"
            "| [cat/three](https://github.com/cat/three) | @cat | [cccccccccc](u) only here | GPL-3.0 | 2026-10-06 |\n")
        return hub

    def test_one_person_per_login_across_corpora(self):
        import contributors
        with tempfile.TemporaryDirectory() as d:
            reg = [{"slug": "live-one", "status": "live", "repos": [{"url": "https://github.com/ann/x"}]}]
            people = contributors.load(self._hub(d), reg)
        self.assertEqual(sorted(people), ["Ann", "cat"])
        v = contributors.volume(people["Ann"], {"live-one"})
        # 2 distinct commits (the themed row repeats the first), files/lines once each
        self.assertEqual((v["commits"], v["files"], v["lines"]), (2, 5, 150))
        # ann--one, bob--two, themed, and the live corpus pinning a repo she owns
        self.assertEqual(v["corpora"], 4)
        self.assertEqual(v["live"], ["live-one"])
        self.assertEqual(v["gh_stars"], 15)
        self.assertEqual(v["since"], "2026-10-05")
        self.assertEqual(contributors.volume(people["cat"])["commits"], 1)
        # the ledger split under the 5 MB file limit (CONTRIBUTORS.jsonl + CONTRIBUTORS-2.jsonl) loads the same people
        with tempfile.TemporaryDirectory() as d2:
            hub = self._hub(d2); a = hub / "candidates" / "authors"
            first, second = (a / "CONTRIBUTORS.jsonl").read_text().splitlines()
            (a / "CONTRIBUTORS.jsonl").write_text(first + "\n"); (a / "CONTRIBUTORS-2.jsonl").write_text(second + "\n")
            split = contributors.load(hub, reg)
        self.assertEqual(sorted(split), sorted(people))
        self.assertEqual(contributors.volume(split["Ann"], {"live-one"})["corpora"], 4)
        idx = contributors.by_corpus(people)
        self.assertEqual(idx["themed"], ["Ann", "cat"])
        self.assertEqual(idx["live-one"], ["Ann"])
        html = contributors.corpus_section(idx["themed"], gen.esc, people)
        self.assertIn('href="../contributors/Ann"', html)
        self.assertEqual(contributors.corpus_section([], gen.esc, people), "")

    def test_one_shared_badge_file_serves_every_readme_badge_url(self):
        # Cloudflare Pages takes at most 20,000 files; a badge copy per login broke deploys at ~6,300 contributors
        self.assertIn("/contributors/:login/badge.svg /contributors/badge.svg 200", gen.REDIRECTS)
        import contributors
        self.assertIn("contributors/{login}/badge.svg", open(contributors.__file__).read())   # README markdown URL unchanged

    def test_pinned_repo_stars_are_distinct_and_never_estimated(self):
        import contributors
        with tempfile.TemporaryDirectory() as d:
            people = contributors.load(self._hub(d))
        # Ann/one (10) is reached twice (ledger + themed row) and counts once;
        # cat/three comes only from the themed table, has no count, and is left out
        self.assertEqual(contributors.pinned_repo_stars(people), (15, 2))
        self.assertEqual([contributors.compact(n) for n in (950, 48_200, 5_501_659, 12_400_000, 2_000_000)],
                         ["950", "48.2K", "5.5M", "12.4M", "2M"])

    def test_no_licence_is_marked_reviewed(self):
        import contributors
        with tempfile.TemporaryDirectory() as d:
            people = contributors.load(self._hub(d))
        self.assertFalse(any(w["reviewed"] for p in people.values() for w in p["work"].values()))

    def test_card_says_a_corpus_in_review_is_not_installable(self):
        # issue #1890: the page named a slug whose manifest 404s, with no warning.
        import contributors
        with tempfile.TemporaryDirectory() as d:
            people = contributors.load(self._hub(d))
        html = contributors.person_body(people["Ann"], gen.esc, set())
        self.assertIn("Not installable yet", html)
        live = contributors.person_body(people["Ann"], gen.esc, {"ann--one", "bob--two", "themed"})
        self.assertNotIn("Not installable yet", live)

    def test_page_says_the_corpus_is_the_whole_repo(self):
        # issue #1894: "9 files, 809 lines" read as the corpus size, but the manifest
        # indexes the whole repository at the pin.
        import contributors
        with tempfile.TemporaryDirectory() as d:
            people = contributors.load(self._hub(d))
        html = contributors.person_body(people["Ann"], gen.esc, set())
        self.assertIn("selected commit: 3 files", html)
        self.assertIn("whole repository at the pinned commit", html)

    def test_card_shows_only_the_github_facts_that_exist(self):
        # Search Console: contributor pages "crawled, not indexed" carried only a handle, stars and the
        # commit. Cards now add the repository as GitHub reports it, and nothing GitHub did not return.
        st = {"description": "Modern lobby management", "commits": 23, "forks": 0, "created": "2026-10-01",
              "pushed": "2026-10-18", "languages": [["Java", 99.6], ["Shell", 0.4]],
              "tree": {"files": 41, "bytes": 1, "sha": "a" * 40}}
        h = contributors.repo_facts_html(st, gen.esc)
        self.assertIn("Modern lobby management", h)
        self.assertIn("23 commits · 41 files at the pinned commit · created 2026-10 · last push 2026-10-18", h)
        self.assertIn("Languages: Java 100%", h)
        self.assertNotIn("forks", h)      # zero forks: not worth a line
        self.assertNotIn("Shell", h)      # under 1%
        self.assertEqual(contributors.repo_facts_html({}, gen.esc), "")
        self.assertNotIn("files at", contributors.repo_facts_html({"commits": 3}, gen.esc))
        with tempfile.TemporaryDirectory() as d:
            people = contributors.load(self._hub(d))
        w = next(iter(people["Ann"]["work"].values()))
        html = contributors.person_body(people["Ann"], gen.esc, set(), gh={repo_pages.key(w["repo_url"]): st})
        self.assertIn('class="repofacts"', html)
        self.assertNotIn('class="repofacts"', contributors.person_body(people["Ann"], gen.esc, set()))

    def test_gsc_seen_lists_contributors(self):
        seen = repo_pages.load_seen_contributors()
        self.assertGreater(len(seen), 100)
        self.assertTrue(all("/" not in l and not l.endswith(".html") for l in seen))


class SignInNavTest(unittest.TestCase):
    def test_sign_in_box_ships_hidden_and_asks_api_me(self):
        html = gen.page("t", "d", "<main></main>")
        # hidden in the static HTML: an anonymous visitor on a hub without the
        # OAuth secrets sees exactly the page they saw before
        self.assertIn('<details class="authbox" id="hubauth" hidden></details>', html)
        self.assertIn("fetch('/api/me'", html)
        self.assertIn("if(!box||!me||!me.ok||!me.enabled)return;", html)
        # user-controlled text is never written as HTML
        self.assertNotIn("innerHTML", gen.AUTH_JS)

    def test_me_page_is_an_empty_shell(self):
        # personal data only ever arrives from the authenticated API
        self.assertIn("/api/v1/me/stars", gen.ME_JS)
        self.assertNotIn("innerHTML", gen.ME_JS)


class UsageSectionTest(unittest.TestCase):
    """The usage block carries no number of its own: every figure comes from
    /api/hub-usage at page load, and the empty state is stated, not a zero."""

    def test_corpus_section_has_no_baked_in_numbers(self):
        import re
        import usage
        html = usage.corpus_section('lz4"><x', gen.esc)
        self.assertIn('data-usage="lz4&quot;&gt;&lt;x"', html)
        for m in re.finditer(r'<b data-u="\w+">([^<]*)</b>', html):
            self.assertEqual(m.group(1), "&ndash;")
        self.assertIn("No usage recorded yet", usage.USAGE_JS)
        t = usage.traffic_section("lz4", gen.esc)
        self.assertIn('data-traffic="lz4"', t)
        for m in re.finditer(r'<b data-t="\w+">([^<]*)</b>', t):
            self.assertEqual(m.group(1), "&ndash;")
        self.assertIn("No traffic recorded yet", usage.USAGE_JS)
        self.assertNotIn("innerHTML", usage.USAGE_JS)

    def test_privacy_note_states_what_is_and_is_not_stored(self):
        import usage
        for phrase in ("raw query", "IP address", "3 times", "X-Xerj-Disable-Feedback", "changes every\nday", "host name"):
            self.assertIn(phrase, usage.PRIVACY)
        self.assertIn('id="privacy"', gen.WHATS_THIS)


class PrivateDataTest(unittest.TestCase):
    """The signed-in details (private_data.py) carry no consent data and never land in out/."""

    def _hub(self, d):
        hub = pathlib.Path(d) / "hub"
        (hub / "candidates" / "authors").mkdir(parents=True)
        row = {"corpus": "ann--one", "repo": "ann/one", "repo_url": "https://github.com/ann/one",
               "contributor": {"login": "Ann", "name": "Ann A", "email": "ann@example.com"},
               "consent": {"at": "2026-10-05T10:00Z", "channel": "email reply", "scope": "s", "from": "ann@example.com"},
               "contribution": {"commit": "a" * 40, "commit_url": "https://github.com/ann/one/commit/" + "a" * 40,
                                "title": "feat: x", "committed_at": "2026-10-01T00:00:00Z", "files": 3, "lines": 90,
                                "areas": ["src/a", "src/b"], "langs": ["Go"]},
               "pin": {"sha": "a" * 40}, "licence": {"spdx": "MIT", "suggested_use": "adapt-with-attribution"}}
        (hub / "candidates" / "authors" / "CONTRIBUTORS.jsonl").write_text(json.dumps(row) + "\n")
        (hub / "candidates" / "authors" / "ann--one.json").write_text(json.dumps({"corpus": "ann--one", "status": "draft"}))
        root = pathlib.Path(d)
        (root / "benchmarks" / "hub-backfill").mkdir(parents=True)
        rows = [{"run": "2", "date": "2026-10-19", "slug": "live-one", "pin8": "bbbbbbbb", "result": "published", "tag": "pack-live-one-bbbbbbbb", "bytes": 10, "defs": 5, "url": "u2"},
                {"run": "1", "date": "2026-10-18", "slug": "live-one", "pin8": "cccccccc", "result": "failed", "tag": "", "error": "e2e", "url": "u1"}]
        (root / "benchmarks" / "hub-backfill" / "rows.jsonl").write_text("".join(json.dumps(r) + "\n" for r in rows))
        return root, hub

    def test_details_shape_and_no_consent(self):
        import contributors
        import private_data
        with tempfile.TemporaryDirectory() as d:
            root, hub = self._hub(d)
            reg = [{"slug": "live-one", "lane": "A", "status": "live", "repos": [{"repo": "one", "url": "https://github.com/ann/one", "sha": "b" * 40,
                    "review": {"spdx": "MIT", "use": "adapt-with-attribution", "by": "xerj-org", "at": "2026-10-13", "note": "ok"}}]}]
            people = contributors.load(hub, reg)
            data = private_data.build(root, hub, root / "packs", reg, people, "2026-10-19")
            raw = private_data.module(data)
        self.assertNotIn("example.com", raw)
        self.assertNotIn("email reply", raw)
        w = data["contributors"]["ann"]["work"][0]
        self.assertEqual((w["committed_at"], w["areas"], w["files"]), ("2026-10-01T00:00:00Z", ["src/a", "src/b"], 3))
        self.assertNotIn("commit_url", w)                  # derivable, rebuilt by the API
        self.assertEqual(w["corpora"][0]["manifest"], "draft")
        c = data["corpora"]["live-one"]
        self.assertEqual([b["run"] for b in c["builds"]], ["2", "1"])     # newest first
        self.assertEqual([p["pin8"] for p in c["pins"]], ["cccccccc", "bbbbbbbb"])
        self.assertEqual(c["pins"][1]["published"], "pack-live-one-bbbbbbbb")
        self.assertEqual(c["repos"][0]["review"]["by"], "xerj-org")
        self.assertEqual(data["contributors"]["ann"]["also_pinned_in"], {"live-one": "live"})
        self.assertTrue(raw.startswith("// GENERATED"))

    def test_details_never_written_to_the_site(self):
        self.assertIn('export default null;', private_data_stub())


def private_data_stub():
    import private_data
    return private_data.STUB


class AgentScorePanelTest(unittest.TestCase):
    """The Agent score is its own section, never mixed with hub stars, and
    ships with no number in the HTML (the API fills it, or says there is none)."""

    def test_panel_is_separate_and_honest(self):
        import agent_score
        html = agent_score.section("Tokio", gen.esc)
        self.assertIn('id="agent-score"', html)
        self.assertIn('data-corpus="tokio"', html)
        self.assertIn("not</b> the human hub star count", html)
        self.assertNotIn("★", html)
        self.assertIn("No agent reviews yet", agent_score.PANEL_JS)
        self.assertIn("not being collected yet", agent_score.PANEL_JS)
        self.assertNotIn("innerHTML", agent_score.PANEL_JS + agent_score.CARDS_JS)

    def test_cards_carry_the_slug_for_the_agent_sort(self):
        c = {"slug": "Tokio", "cat": "DB", "status": "live", "lane": "A", "usecase": "x", "repos": [],
             "tags": {}, "kind": "reference corpus"}
        html = gen.card(c, {}, {}, {})
        self.assertIn('data-slug="tokio"', html)



class NotFoundPageTest(unittest.TestCase):
    # issue #1895: without a 404.html, Pages answers every unknown path with the
    # homepage and a 200.
    def test_404_page_is_generated_with_absolute_links(self):
        html = gen.page("Not found: XERJ Corpus Hub", "This page does not exist", gen.NOT_FOUND_BODY, rel="/")
        self.assertIn("Not found", html)
        self.assertIn('href="/assets/style.css', html)
        self.assertNotIn('href="assets/', html)
        self.assertIn('"404.html"', pathlib.Path(gen.__file__).read_text())
class NavTest(unittest.TestCase):
    # owner request 2026-10-20: no Domains / Registry (git) links; About became "What's this?"
    def test_nav_and_footer(self):
        html = gen.page("t", "d", "<main></main>", rel="../")
        self.assertNotIn("DOMAINS", html)
        self.assertNotIn("#domains", html)
        self.assertNotIn("REGISTRY (GIT)", html)
        self.assertNotIn("about.html", html)
        self.assertEqual(html.count('href="../whats-this/">WHAT\'S THIS?</a>'), 2)   # nav + footer

    def test_whats_this_says_what_the_hub_is_for(self):
        w = gen.WHATS_THIS
        for fact in ("pre-indexed", "signed", "file:line", "agent score", "profile", "hub stars", 'id="privacy"'):
            self.assertIn(fact, w)
        self.assertIn("/about.html /whats-this/ 301", gen.REDIRECTS)
        self.assertIn("/about /whats-this/ 301", gen.REDIRECTS)


class BusyPortStepTest(unittest.TestCase):
    # issue #1891 was about the v1 manual-node step. In v2 XERJ finds or starts
    # its own node on a free port, so the prompts carry the node rule instead:
    # which node --url / $XERJ_URL picks, and stopping only the node you started.
    def test_node_rule_names_the_url_and_stopping_only_your_node(self):
        self.assertIn("--url", gen.NODE_NOTE)
        self.assertIn("$XERJ_URL", gen.NODE_NOTE)
        self.assertIn("xerj hub stop", gen.NODE_NOTE)
        self.assertIn("Never pkill or killall xerj", gen.NODE_NOTE)

def _contrast(a, b):
    def lum(h):
        c = [int(h[i:i + 2], 16) / 255 for i in (1, 3, 5)]
        c = [x / 12.92 if x <= 0.04045 else ((x + 0.055) / 1.055) ** 2.4 for x in c]
        return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
    hi, lo = sorted((lum(a), lum(b)), reverse=True)
    return (hi + 0.05) / (lo + 0.05)


class TreemapTest(unittest.TestCase):
    """The /contributors/ star treemap (treemap.py)."""

    def _items(self, stars):
        return [{"names": [f"User Number {i}", f"user{i}"], "stars": s, "stars_txt": f"★ {s:,}", "href": f"user{i}.html",
                 "avatar": f"user{i}", "title": f"user{i}"} for i, s in enumerate(stars)]

    # long tail like the real data: a few huge, a long run of small ones
    TAIL = [400000, 300000] + [50000 - 900 * i for i in range(50)] + [5000 - 4 * i for i in range(300)] + [300] * 300 + [3] * 400 + [0] * 5

    def test_squarify_areas_are_proportional_and_fill_the_box(self):
        import treemap
        vals = [500, 300, 120, 80, 40, 9, 1]
        for aspect in (1.0, 2.5):
            rects = treemap.squarify(vals, 1040, 720, aspect)
            total = sum(vals)
            for v, (x, y, w, h) in zip(vals, rects):
                self.assertAlmostEqual(w * h, v / total * 1040 * 720, delta=1e-6 * 1040 * 720)
                self.assertGreaterEqual(x, -1e-6)
                self.assertGreaterEqual(y, -1e-6)
                self.assertLessEqual(x + w, 1040 + 1e-6)
                self.assertLessEqual(y + h, 720 + 1e-6)
            self.assertAlmostEqual(sum(w * h for _, _, w, h in rects), 1040 * 720, places=3)

    def test_every_top_tile_fits_a_14px_one_line_name(self):
        import treemap
        items = self._items(self.TAIL)
        for key, W, H, aspect, _, _ in treemap.LAYOUTS:
            tiles = treemap.fit(items, W, H, aspect)
            self.assertTrue(tiles, key)
            self.assertLess(len(tiles), len(items))        # the rest goes to the flow tail, not squeezed
            self.assertEqual([t[0]["href"] for t in tiles], [i["href"] for i in items[:len(tiles)]])
            for it, (x, y, w, h), lab in tiles:
                self.assertGreaterEqual(lab["fs"], 14)
                self.assertGreaterEqual(h - treemap.GAP, treemap.MIN_H)
                self.assertLessEqual(treemap.text_w(lab["text"], lab["fs"]), w - treemap.GAP)
                self.assertIn(lab["text"], it["names"])

    def test_render_shows_up_to_the_cap_then_one_others_tile(self):
        # owner request: ~800 readable tiles, then one "N others" tile opening the table
        import treemap
        items = self._items(self.TAIL)
        html, info = treemap.render(items, gen.esc, "people", total=len(items))
        self.assertEqual(set(info), {"l", "m", "s"})
        pos = sum(1 for s in self.TAIL if s > 0)
        for key, _, _, _, cap, _ in treemap.LAYOUTS:
            top, shown, others = info[key]
            self.assertEqual(shown, min(cap, pos))
            self.assertEqual(others, len(items) - shown)
            self.assertIn(f'data-from-{key}="{shown + 1}"', html)
            self.assertIn(f"{others:,} others", html)
        self.assertEqual(html.count('class="tmo"'), 1)
        # each rank is drawn once per layout: band 0 or the tail (tail tiles hidden where band 0 has them)
        l_top, l_shown, _ = info["l"]
        tail = re.findall(r'<li class="([^"]*)" style="flex-grow', html)
        self.assertEqual(sum(1 for c in tail if "tmx-l" not in c), l_shown - l_top)
        self.assertNotIn("<script", html)
        self.assertIn('loading="lazy"', html)

    def test_tail_tiles_are_never_narrower_than_their_label(self):
        import treemap
        css = treemap.STYLE
        self.assertIn("flex-basis:auto", css)                  # a flex item is at least its content wide...
        self.assertIn("white-space:nowrap", css)               # ...and the label never wraps
        self.assertIn(f"font-size:{treemap.XS_FONT}px", css)
        self.assertGreaterEqual(treemap.XS_FONT, 12)

    def test_tiles_have_no_borders_or_shadows(self):
        # owner review of #1955: tiles are separated by a gap of page background only
        import treemap
        self.assertEqual(treemap.BORDER, 0)
        self.assertGreaterEqual(treemap.GAP, 2)
        css = treemap.STYLE
        self.assertIn(".tmap li{position:absolute;overflow:hidden;border:0;", css)
        import contributors
        for bad in ("box-shadow", "gradient", "solid rgba"):
            self.assertNotIn(bad, css)
            self.assertNotIn(bad, contributors.TABLE_STYLE)

    def test_tile_and_table_colours_are_aaa(self):
        import treemap
        import contributors
        tints = set(treemap.TINTS) | {treemap.OTHER_TINT} | set(re.findall(r"background:(#[0-9a-f]{6})", contributors.TABLE_STYLE))
        for bg in tints:
            self.assertGreaterEqual(_contrast(treemap.INK, bg), 7.0, bg)
            self.assertGreaterEqual(_contrast(treemap.MUTE, bg), 7.0, bg)


class SiteLinksTest(unittest.TestCase):
    """Builds the whole site once into a temp dir and checks it (issue: corpus
    pages and contributor pages were not linked to each other)."""

    HREF = re.compile(r'\bhref="([^"]*)"')

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        orig = gen.OUT
        gen.OUT = pathlib.Path(cls.tmp.name) / "out"
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                gen.main()
        finally:
            cls.out, gen.OUT = gen.OUT, orig

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def _resolves(self, page, href):
        u = urllib.parse.urlsplit(href)
        if u.scheme or u.netloc or not u.path:
            return True                                   # external, or a #fragment on the same page
        path = urllib.parse.unquote(u.path)
        t = pathlib.Path(os.path.normpath((self.out / path.lstrip("/")) if path.startswith("/") else (page.parent / path)))
        return t.is_file() or (t / "index.html").is_file() or t.with_name(t.name + ".html").is_file()

    def _pages(self, sub):
        return sorted((self.out / sub).glob("*.html"))

    def test_every_corpus_page_links_a_contributor_and_every_link_resolves(self):
        pages = self._pages("corpus")
        self.assertGreater(len(pages), 500)
        bad, lonely = [], []
        for f in pages:
            hrefs = self.HREF.findall(f.read_text())
            people = [h for h in hrefs if re.search(r"contributors/[^/?#]", h)]
            if not people:
                lonely.append(f.name)
            bad += [(f.name, h) for h in hrefs if not self._resolves(f, h)]
        self.assertEqual(lonely, [], "corpus pages without a contributor link")
        self.assertEqual(bad, [], "broken links on corpus pages")

    def test_every_contributor_page_links_its_corpora_and_every_link_resolves(self):
        bad, lonely = [], []
        for f in self._pages("contributors"):
            html = f.read_text()
            hrefs = self.HREF.findall(html)
            bad += [(f.name, h) for h in hrefs if not self._resolves(f, h)]
            if f.stem != "index" and not f.stem.startswith("list.") and not any(h.startswith("../corpus/") for h in hrefs):
                lonely.append(f.name)
        self.assertEqual(lonely, [], "contributor pages that link no corpus")
        self.assertEqual(bad, [], "broken links on contributor pages")

    def test_list_chunks_cover_every_contributor_once_in_wall_order(self):
        import contributors
        d = self.out / "contributors"
        html = (d / "index.html").read_text()
        people = html.split('<table class="ctab" data-mode="people"', 1)[1].split("</table>", 1)[0]
        first = re.findall(r'<td class="who"><a href="([^"]+)">', people)
        self.assertEqual(len(first), contributors.CHUNK)
        logins = []
        for f in sorted(d.glob("list.*.json"), key=lambda p: int(p.name.split(".")[1])):
            rows = json.loads(f.read_text())
            self.assertLessEqual(len(rows), contributors.CHUNK)
            self.assertTrue(all(len(r) == 7 for r in rows))
            logins += [r[0] for r in rows]
            if f.name != "list.1.json":                                     # index.html is page 1
                self.assertTrue((d / f.name.replace(".json", ".html")).is_file())   # crawlable twin
        self.assertEqual(logins[:contributors.CHUNK], first)
        self.assertEqual(len(logins), len(set(logins)))
        self.assertTrue(all((d / f"{l}.html").is_file() for l in logins))
        stars = [r[4] for r in json.loads((d / "list.1.json").read_text())]
        self.assertEqual(stars, sorted(stars, reverse=True))
        self.assertIn('class="pager"', html)                                   # plain links without JS
        self.assertIn('class="tmap tm tm-l"', html)
        # hub stars sit in their own Hub ★ column, filled by STARS_JS, not as a '· hub ★ n' line under the name
        self.assertIn('<th scope="col" class="num o1 hub">Hub ★</th>', html)
        self.assertEqual(people.count('<td class="num o1 hub hs"></td>'), contributors.CHUNK)
        self.assertNotIn("· hub ★", contributors.STARS_JS + contributors.TABLE_JS)

    def test_repos_mode_map_and_table_cover_every_starred_repo_once(self):
        # owner request: People | Repos switch; Repos = one tile / row per distinct indexed repo
        import contributors
        d = self.out / "contributors"
        html = (d / "index.html").read_text()
        for needle in ('data-mode="people" aria-pressed="true"', 'data-mode="repos"', 'data-view="map"', 'data-view="table"',
                       '<section class="tmwrap" data-mode="repos"', '<table class="ctab" data-mode="repos"', 'th scope="col"'):
            self.assertIn(needle, html)
        rows = []
        for f in sorted(d.glob("repos.*.json"), key=lambda p: int(p.name.split(".")[1])):
            rows += json.loads(f.read_text())
            self.assertTrue((d / f.name.replace(".json", ".html")).is_file())
        urls = [r[1].rstrip("/").lower() for r in rows]
        self.assertEqual(len(urls), len(set(urls)))
        total, n = contributors.pinned_repo_stars(contributors.load(gen.HUB, gen.load_registry()))
        self.assertEqual(len(rows), n)
        self.assertEqual(sum(r[2] for r in rows), total)
        self.assertEqual([r[2] for r in rows], sorted((r[2] for r in rows), reverse=True))
        for r in rows:
            if r[5]:
                self.assertTrue((self.out / "corpus" / f"{r[5]}.html").is_file(), r)
            if r[4]:
                self.assertTrue((d / f"{r[4]}.html").is_file(), r)
        repos_map = html.split('<section class="tmwrap" data-mode="repos"', 1)[1].split("</section>", 1)[0]
        self.assertIn('class="tmo"', repos_map)
        self.assertIn("others", repos_map)

    def test_categories_list_contributed_corpora(self):
        # owner, 2026-10-29: a contributor's corpus was live but missing from its category page.
        # Every corpus in the contributor ledger is linked from its shelf's category page, to a page that exists.
        import contributors, re as _re
        people = contributors.load(gen.HUB, gen.load_registry())
        want = {}
        for pp in people.values():
            for w in pp["work"].values():
                for slug in w.get("corpora") or []:
                    want.setdefault(slug, set()).add(w.get("shelf"))
        if not want:
            self.skipTest("no contributed corpora")
        listed = {}
        for f in (self.out / "category").glob("*.html"):
            html = f.read_text()
            if 'id="contributed"' in html:
                for slug in set(_re.findall(r'href="\.\./corpus/([^"]+)"', html.split('id="contributed"', 1)[1])):
                    listed.setdefault(slug, set()).add(f.stem)
                    self.assertTrue((self.out / "corpus" / f"{slug}.html").is_file(), slug)
        self.assertEqual(set(want) - set(listed), set())

    def test_home_stats_are_stars_and_contributors(self):
        # owner request: "registry rows" / "live corpora" replaced by real numbers
        import contributors
        html = (self.out / "index.html").read_text()
        strip = html.split('class="statgrid"', 1)[1].split("</div>\n</div>", 1)[0]
        self.assertNotIn("registry rows", strip)
        self.assertNotIn("live corpora", strip)
        people = contributors.load(gen.HUB, gen.load_registry())
        stars, n = contributors.pinned_repo_stars(people)
        self.assertIn(f"<b>{len(people):,}</b><span>contributors</span>", strip)
        self.assertIn(f"<b>{stars:,}</b><span>GitHub stars across pinned repos</span>", strip)   # full numbers, no "7.2M"
        self.assertIn(f"{stars:,} GitHub stars, summed over {n:,} distinct pinned repositories", strip)
        # owner, 2026-10-29: the repository count and the file count (large numbers), not "11 domains" / "78.7 GB"
        n_repos = len({w["repo_url"].rstrip("/").lower() for pp in people.values() for w in pp["work"].values() if w.get("repo_url")})
        n_lines = sum(contributors.volume(pp)["lines"] for pp in people.values())
        self.assertIn(f"<b>{n_lines:,}</b><span>lines of contributed code</span>", strip)
        self.assertNotIn("contributed repositories</span>", strip)
        self.assertNotRegex(strip, r"<b>[\d.]+[KMB]</b>")
        self.assertRegex(strip, r"<b>[\d,]{7,}</b><span>pinned files</span>")
        self.assertNotIn("<span>pinned text</span>", strip)

    def test_whats_this_replaces_about(self):
        self.assertTrue((self.out / "whats-this" / "index.html").is_file())
        self.assertFalse((self.out / "about.html").exists())
        self.assertIn("/about.html /whats-this/ 301", (self.out / "_redirects").read_text())

    def test_no_broken_relative_links_anywhere(self):
        bad = []
        for f in self.out.rglob("*.html"):
            bad += [(str(f.relative_to(self.out)), h) for h in self.HREF.findall(f.read_text()) if not self._resolves(f, h)]
        self.assertEqual(bad, [])


def _ihdr(path):
    data = pathlib.Path(path).read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise AssertionError(f"{path} is not a PNG")
    return struct.unpack(">II", data[16:24])


class DiscoverabilityTest(unittest.TestCase):
    """Search and AI engines can see the hub: canonical origin, social tags,
    JSON-LD, sitemap, robots, favicon, analytics. Internal links are the URL
    Cloudflare Pages serves, so they do not 308."""

    HOME_DESC = ("Pinned, licence-checked, measured corpora an agent downloads "
                 "per task and searches instead of guessing.")

    def _ld(self, html):
        blocks = re.findall(r'<script type="application/ld\+json">\n(.*?)\n</script>', html, re.S)
        self.assertGreaterEqual(len(blocks), 3, "Organization, WebSite, and a page type")
        return [json.loads(b) for b in blocks]

    def test_href_strips_html_the_way_pages_serves_it(self):
        self.assertEqual(gen.href("index.html"), "./")
        self.assertEqual(gen.href("/index.html"), "/")
        self.assertEqual(gen.href("../index.html"), "../")
        self.assertEqual(gen.href("contributors/index.html"), "contributors/")
        self.assertEqual(gen.href("../contributors/index.html"), "../contributors/")
        self.assertEqual(gen.href("corpus/lz4.html"), "corpus/lz4")
        self.assertEqual(gen.href("contribute.html"), "contribute")
        self.assertEqual(gen.href("index.html#repos"), "./#repos")
        self.assertEqual(gen.file_url("index.html"), "https://hub.xerj.org/")
        self.assertEqual(gen.file_url("whats-this/index.html"), "https://hub.xerj.org/whats-this/")
        self.assertEqual(gen.file_url("corpus/lz4.html"), "https://hub.xerj.org/corpus/lz4")
        self.assertEqual(gen.file_url("contributors/list.2.html"), "https://hub.xerj.org/contributors/list.2")

    def test_page_head_has_canonical_social_jsonld_favicon_and_ga4(self):
        html = gen.page("T", "D.", "<main></main>", rel="../", path="/corpus/lz4")
        self.assertEqual(html.count('<link rel="canonical"'), 1)
        self.assertIn('<link rel="canonical" href="https://hub.xerj.org/corpus/lz4">', html)
        self.assertNotIn("pages.dev", html.split("<body>", 1)[0].split("location.hostname")[0])
        self.assertIn('property="og:title"', html)
        self.assertIn('property="og:description" content="D."', html)
        self.assertIn('property="og:url" content="https://hub.xerj.org/corpus/lz4"', html)
        self.assertIn('property="og:image" content="https://hub.xerj.org/assets/og.png"', html)
        self.assertIn(f'property="og:image:width" content="{gen.OG_W}"', html)
        self.assertIn(f'property="og:image:height" content="{gen.OG_H}"', html)
        self.assertIn('name="twitter:card" content="summary_large_image"', html)
        self.assertIn('rel="icon" href="../favicon.png"', html)
        self.assertIn(f'src="https://www.googletagmanager.com/gtag/js?id={gen.GA_ID}"', html)
        self.assertIn("<script async src=", html)
        self.assertIn(f"gtag('config','{gen.GA_ID}')", html)
        self.assertIn('location.hostname==="xerj-hub.pages.dev"', html)
        self.assertIn('location.replace("https://hub.xerj.org"+location.pathname', html)
        types = {b["@type"] for b in self._ld(html)}
        self.assertEqual(types, {"Organization", "WebSite", "WebPage"})
        self.assertIn('href="../"', html)          # CORPORA, extensionless
        self.assertIn('href="../contributors/"', html)
        self.assertNotIn('href="../index.html"', html)
        self.assertNotIn('href="../contribute.html"', html)

    def test_collection_pages_are_collection_pages_and_home_description_ends(self):
        home = gen.page("XERJ Corpus Hub: reference memory for AI agents", self.HOME_DESC,
                        "<main></main>", path="/", kind="CollectionPage")
        self.assertIn(f'<meta name="description" content="{self.HOME_DESC}">', home)
        self.assertIn('"@type": "CollectionPage"', home)
        cat = gen.page("SEC: t", "d", "<main></main>", path="/category/SEC", kind="CollectionPage")
        self.assertIn('"@type": "CollectionPage"', cat)
        self.assertIn('<link rel="canonical" href="https://hub.xerj.org/category/SEC">', cat)

    def test_robots_allows_every_crawler_and_names_the_sitemap(self):
        text = gen.ROBOTS
        self.assertIn("User-agent: *\n", text)
        self.assertIn("Allow: /", text)
        self.assertIn("Content-Signal: search=yes, ai-input=yes, ai-train=yes", text)
        self.assertIn("User-agent: GPTBot\nAllow: /", text)
        self.assertIn("User-agent: ClaudeBot\nAllow: /", text)
        self.assertIn("User-agent: Google-Extended\nAllow: /", text)
        self.assertIn("User-agent: PerplexityBot\nAllow: /", text)
        self.assertIn("Sitemap: https://hub.xerj.org/sitemap.xml\n", text)
        # pages stay open; only sign-in and API endpoints are kept out, in every group
        groups = text.count("User-agent: ")
        self.assertEqual(text.count("Disallow: /auth/\n"), groups)
        self.assertEqual(text.count("Disallow: /api/\n"), groups)
        self.assertEqual(set(re.findall(r"Disallow: (\S+)", text)), {"/auth/", "/api/"})

    def test_og_card_is_the_light_wordmark_on_the_hub_background(self):
        import tempfile
        with tempfile.TemporaryDirectory() as d:
            gen.write_brand_assets(d)
            root = pathlib.Path(d)
            self.assertEqual(_ihdr(root / "favicon.png"), (128, 128))
            self.assertEqual(_ihdr(root / "assets" / "hub-logo.png"), (512, 512))
            self.assertEqual(_ihdr(root / "assets" / "og.png"), (gen.OG_W, gen.OG_H))
            _w, _h, rows = gen.read_png(root / "assets" / "og.png")
            # corner is the page background; the wordmark sits in the middle
            self.assertEqual(tuple(rows[0][:4]), (0xFA, 0xFA, 0xF7, 255))
            mid = rows[_h // 2]
            self.assertEqual(tuple(mid[(_w // 2) * 4:(_w // 2) * 4 + 4]), (255, 255, 255, 255))


class AgentIndexFilesTest(unittest.TestCase):
    """llms.txt carries a build date, and its two companion indexes and the
    IndexNow key file are produced from the registry on every build."""

    def test_stamp_is_a_date(self):
        self.assertRegex(gen.STAMP, r"^\d{4}-\d{2}-\d{2}$")

    def test_hub_stamp_env_wins(self):
        old = os.environ.get("HUB_STAMP")
        os.environ["HUB_STAMP"] = "2030-01-02"
        try:
            self.assertEqual(gen.build_stamp(), "2030-01-02")
        finally:
            if old is None:
                os.environ.pop("HUB_STAMP")
            else:
                os.environ["HUB_STAMP"] = old

    def test_llms_repos_one_line_per_pin(self):
        live = [{"slug": "b-corpus", "repos": [{"url": "https://github.com/o/b", "sha": "a" * 40,
                                                 "review": {"spdx": "MIT"}}]},
                {"slug": "a-corpus", "repos": [{"url": "https://github.com/o/a", "sha": "",
                                                 "licence": "Apache-2.0"},
                                                {"url": "https://github.com/o/a", "sha": ""}]}]
        out = gen.llms_repos(live)
        rows = [x for x in out.splitlines() if x.startswith("- ")]
        self.assertEqual(len(rows), 2)
        self.assertIn("corpus a-corpus: https://hub.xerj.org/corpus/a-corpus", rows[0])
        self.assertIn("@ aaaaaaaaaaaa · MIT", rows[1])
        self.assertIn("2 repository pins across 2 live corpora", out)
        self.assertNotRegex(out, "[\u2013\u2014]")

    def test_llms_contributors_counts_live_corpora(self):
        people = {"zed": {"name": "Zed", "work": {"x": {"commit": "c1", "repo_url": "https://github.com/z/z"}},
                          "corpora": {"live-one", "cand"}, "agreed": ["2026-10-01"]}}
        out = gen.llms_contributors(people, {"live-one"})
        self.assertIn("- Zed (@zed): https://hub.xerj.org/contributors/zed · 1 selected commit · 1 live corpus: live-one", out)


class BuiltSiteDiscoverabilityTest(SiteLinksTest):
    """The real registry build: every indexable canonical is in the sitemap,
    and no internal link still asks for a .html URL."""

    def test_sitemap_matches_canonicals_and_skips_noindex(self):
        idx = (self.out / "sitemap.xml").read_text()
        self.assertTrue(idx.startswith('<?xml version="1.0" encoding="UTF-8"?>'))
        self.assertIn("<sitemapindex ", idx)
        children = re.findall(r"<loc>([^<]+)</loc>", idx)
        self.assertEqual([c.rsplit("/", 1)[1] for c in children],
                         [f"sitemap-{k}.xml" for k, _ in gen.SITEMAPS if (self.out / f"sitemap-{k}.xml").exists()])
        locs = []
        for c in children:
            sm = (self.out / c.rsplit("/", 1)[1]).read_text()
            self.assertTrue(sm.startswith('<?xml version="1.0" encoding="UTF-8"?>'))
            self.assertNotIn("<priority>", sm)
            self.assertNotIn("<changefreq>", sm)
            mine = re.findall(r"<loc>([^<]+)</loc>", sm)
            self.assertEqual(mine, sorted(mine))
            for lm in re.findall(r"<lastmod>([^<]+)</lastmod>", sm):
                self.assertRegex(lm, r"^\d{4}-\d{2}-\d{2}$")
                self.assertLessEqual(lm, gen.dt.date.today().isoformat())
            locs += mine
        self.assertEqual(len(locs), len(set(locs)))
        self.assertGreater(len(locs), 500)
        seen = set()
        for f in self.out.rglob("*.html"):
            rel = f.relative_to(self.out).as_posix()
            html = f.read_text()
            canon = re.findall(r'<link rel="canonical" href="([^"]+)">', html)
            self.assertEqual(canon, [gen.file_url(rel)], rel)
            self.assertNotIn("xerj-hub.pages.dev/", canon[0])
            self.assertIn(f"gtag('config','{gen.GA_ID}')", html)
            types = {json.loads(b)["@type"] for b in re.findall(
                r'<script type="application/ld\+json">\n(.*?)\n</script>', html, re.S)}
            self.assertTrue({"Organization", "WebSite"} <= types, rel)
            noindex = 'name="robots"' in html.split("</head>", 1)[0] and "noindex" in html.split("</head>", 1)[0]
            if rel == "404.html" or noindex:
                self.assertNotIn(canon[0], locs, rel)
            else:
                self.assertIn(canon[0], locs, rel)
                seen.add(canon[0])
            if rel in ("index.html", "category/SEC.html", "contributors/index.html"):
                self.assertIn("CollectionPage", types, rel)
            elif rel.startswith("corpus/"):
                self.assertIn("WebPage", types, rel)
        self.assertEqual(set(locs), seen)
        self.assertIn("https://hub.xerj.org/", locs)
        self.assertIn("https://hub.xerj.org/whats-this/", locs)
        self.assertNotIn("https://hub.xerj.org/404", locs)

    def _meta(self, rel):
        h = (self.out / rel).read_text()
        t = html.unescape(re.search(r"<title>(.*?)</title>", h).group(1))
        d = html.unescape(re.search(r'<meta name="description" content="([^"]*)"', h).group(1))
        return h, t, d

    def test_titles_and_descriptions_fit_are_unique_and_have_no_dashes(self):
        titles, descs, hanging = collections.Counter(), collections.Counter(), []
        for f in self.out.rglob("*.html"):
            rel = f.relative_to(self.out).as_posix()
            h, t, d = self._meta(rel)
            if 'name="robots"' in h.split("</head>", 1)[0] and "noindex" in h.split("</head>", 1)[0]:
                continue
            if rel.startswith(("corpus/", "contributors/")):
                self.assertLessEqual(len(t), 62, rel)
                self.assertLessEqual(len(d), 160, rel)
                self.assertGreaterEqual(len(d), 40, rel)
                self.assertFalse(d.lower().startswith("an agent"), rel)
                self.assertNotIn("XERJ Corpus Hub\u201d", t)
            for x in (t, d):
                self.assertNotIn("\u2014", x, rel)
                self.assertNotIn("\u2013", x, rel)
            titles[t] += 1
            descs[d] += 1
            if repo_pages.dangling(t):
                hanging.append((rel, t))
        self.assertEqual(hanging, [])
        self.assertEqual([t for t, n in titles.items() if n > 1], [])
        self.assertEqual([d for d, n in descs.items() if n > 1], [])

    def test_every_json_ld_block_parses(self):
        for f in self.out.rglob("*.html"):
            for b in re.findall(r'<script type="application/ld\+json">\n(.*?)\n</script>', f.read_text(), re.S):
                json.loads(b)

    def test_repo_page_has_the_sections_the_prompt_and_structured_data(self):
        rel = "corpus/jacobjoergensen--laravel-paper.html"
        h, t, d = self._meta(rel)
        self.assertTrue(t.startswith("JacobJoergensen/laravel-paper: "), t)
        self.assertTrue(d.startswith("laravel-paper: "), d)
        for s in ("What XERJ indexed", "Licence", "Selected work", "Example questions", "Related",
                  "A new way for AI to search for data", ">Copy the prompt</button>", "View on GitHub",
                  'aria-label="Breadcrumb"', 'href="../contributors/JacobJoergensen"'):
            self.assertIn(s, h, s)
        prompt = html.unescape(re.search(r'<pre class="rpprompt">(.*?)</pre>', h, re.S).group(1))
        self.assertIn("git clone https://github.com/JacobJoergensen/laravel-paper laravel-paper", prompt)
        self.assertRegex(prompt, r"git -C laravel-paper checkout [0-9a-f]{40}")
        self.assertIn("xerj autoindex ./laravel-paper --url http://127.0.0.1:9200", prompt)
        self.assertIn("ax_path", prompt)
        types = set()
        for b in re.findall(r'<script type="application/ld\+json">\n(.*?)\n</script>', h, re.S):
            def walk(x):
                if isinstance(x, dict):
                    if "@type" in x:
                        types.add(x["@type"])
                    for v in x.values():
                        walk(v)
                elif isinstance(x, list):
                    for v in x:
                        walk(v)
            walk(json.loads(b))
        self.assertTrue({"Dataset", "SoftwareSourceCode", "BreadcrumbList", "Person"} <= types, types)

    def test_weak_repo_pages_are_noindexed_and_left_out_of_the_sitemap(self):
        sm = set(re.findall(r"<loc>https://hub.xerj.org/corpus/([^<]+)</loc>", (self.out / "sitemap-repos.xml").read_text()))
        seen = repo_pages.load_seen()
        noindexed = enriched = 0
        for f in (self.out / "corpus").glob("*--*.html"):
            h = f.read_text()
            if 'class="rp"' not in h:
                continue
            head = h.split("</head>", 1)[0]
            if '<meta name="robots" content="noindex,follow">' in head:
                noindexed += 1
                self.assertNotIn(f.stem, sm, f.stem)
                self.assertNotIn(f.stem, seen, f.stem)
                self.assertNotIn("Using XERJ with", h)
            else:
                enriched += 1
                self.assertIn(f.stem, sm, f.stem)
                self.assertIn("<h2>Contributors</h2>", h, f.stem)
                self.assertRegex(h, r'href="https://xerj.org/(answers|compare)/', f.stem)
                self.assertNotIn("\u2014", h.split("<h2>Inside the indexed corpus</h2>", 1)[-1])
        self.assertGreater(enriched, 0)
        h = (self.out / "corpus/jacobjoergensen--laravel-paper.html").read_text()
        for s in ("Inside the indexed corpus", "Licence on GitHub and in the corpus", "Using XERJ with laravel-paper",
                  "https://xerj.org/answers/audit-php-codebase-with-an-index"):
            self.assertIn(s, h, s)
        # contributor and registry corpus pages are never noindexed by this rule
        self.assertNotIn('content="noindex', (self.out / "contributors/JacobJoergensen.html").read_text().split("</head>", 1)[0])
        self.assertNotIn('content="noindex', (self.out / "corpus/exploit-pocs-2024.html").read_text().split("</head>", 1)[0])

    def test_no_repo_pages_for_the_exploit_poc_aggregates(self):
        reg = {c["slug"]: c for c in gen.load_registry()}
        pocs = {repo_pages.key(r["url"]) for s, c in reg.items() if s.startswith("exploit-pocs") for r in c["repos"]}
        sm = (self.out / "sitemap-repos.xml").read_text()
        for loc in re.findall(r"<loc>https://hub.xerj.org/corpus/([^<]+)</loc>", sm):
            owner, _, repo = loc.partition("--")
            h = (self.out / "corpus" / f"{loc}.html").read_text()
            m = re.search(r'<a class="cta2" href="([^"]+)"', h)
            if m:
                self.assertNotIn(repo_pages.key(m.group(1)), pocs, loc)
        self.assertIn("exploit-pocs-2024: ", self._meta("corpus/exploit-pocs-2024.html")[1])

    def test_contributor_titles_keep_the_name_and_the_cta_copies_the_prompt(self):
        people = contributors.load(gen.HUB, gen.load_registry())
        kept = total = 0
        for login, pp in people.items():
            f = self.out / "contributors" / f"{login}.html"
            if not pp.get("name") or pp["name"] == login or not f.exists():
                continue
            total += 1
            kept += html.unescape(re.search(r"<title>(.*?)</title>", f.read_text()).group(1)).startswith(pp["name"])
        self.assertGreater(total, 1000)
        self.assertGreater(kept / total, 0.99, (kept, total))
        h = (self.out / "contributors" / "JacobJoergensen.html").read_text()
        self.assertIn(">Copy the prompt</button>", h)
        self.assertIn("xerj autoindex", h)

    def test_discussion_2383_agilepool_is_indexed(self):
        """Yiming1997/agilePool, restored from discussion #2383.

        The ledger row and the candidate manifest are what the generator reads.
        The contributor page, the corpus page, the search index, llms-contributors.txt
        and the sitemap are the pages a crawler and an agent actually fetch.
        """
        pin = "f8ec741dfac7acf201a981d61f93bb11b4bf20bf"
        slug = "yiming1997--agilepool"
        manifest = json.loads((gen.HUB / "candidates" / "authors" / f"{slug}.json").read_text())
        self.assertEqual(manifest["repos"][0]["sha"], pin)
        self.assertEqual(manifest["repos"][0]["review"]["spdx"], "MIT")
        self.assertEqual(manifest["repos"][0]["review"]["use"], "adapt-with-attribution")
        self.assertIn("discussioncomment-19172478", manifest["repos"][0]["review"]["note"])
        people = contributors.load(gen.HUB, gen.load_registry())
        self.assertIn("Yiming1997", people)
        work = next(iter(people["Yiming1997"]["work"].values()))
        self.assertTrue(work["reviewed"])
        self.assertEqual(work["spdx"], "MIT")
        self.assertEqual(work["pin"], pin)
        self.assertIn("discussioncomment-19172478", work["channel"])
        person = (self.out / "contributors" / "Yiming1997.html").read_text()
        corpus = (self.out / "corpus" / f"{slug}.html").read_text()
        self.assertIn(f'href="https://hub.xerj.org/contributors/Yiming1997"', person)
        self.assertIn(f'href="../corpus/{slug}"', person)
        self.assertIn("agreed 2026-10-25", person)
        self.assertIn("Yiming1997/agilePool", corpus)
        self.assertIn('href="../contributors/Yiming1997"', corpus)
        self.assertIn(pin, corpus)
        self.assertIn("The author agreed in a GitHub discussion on 2026-10-25.", corpus)
        self.assertIn("licence cleared", corpus); self.assertNotIn("in licence review", corpus)
        self.assertIn(f"https://hub.xerj.org/contributors/Yiming1997", (self.out / "sitemap-contributors.xml").read_text())
        self.assertIn(f"https://hub.xerj.org/corpus/{slug}", (self.out / "sitemap-repos.xml").read_text())
        llms = (self.out / "llms-contributors.txt").read_text()
        self.assertIn(f"- yiming (@Yiming1997): https://hub.xerj.org/contributors/Yiming1997 · 1 selected commit · 0 live corpora", llms)
        search = json.loads((self.out / "search" / "index.json").read_text())
        row = next(r for r in search["c"] if r[1] == "Yiming1997/agilePool")
        self.assertEqual(row[3], "API")
        self.assertEqual(row[4], "review")
        self.assertTrue(any(r[0] == "Yiming1997" for r in search["p"]))
        # an email consent still says email; the discussion wording is only for a non-email channel
        self.assertIn("agreed by email", (self.out / "corpus" / "jacobjoergensen--laravel-paper.html").read_text())

    def test_add_page_and_links(self):
        """/add: owners sign in with GitHub and add their own repositories (functions/api/v1/me/repos.js).
        Contributor pages and /contribute link to it."""
        add = (self.out / "add.html").read_text()
        self.assertIn("Add your repositories", add)
        self.assertIn("/api/v1/me/repos", add)
        self.assertIn("api.github.com/users/", add)
        self.assertIn("/auth/github/login?return=%2Fadd", add)
        self.assertIn('Own more repositories? <a href="/add">', (self.out / "contributors" / "Yiming1997.html").read_text())
        self.assertIn('<a href="/add">Sign in with GitHub and add it</a>', (self.out / "contribute.html").read_text())
        self.assertIn("Included with the author&#39;s permission (email,", (self.out / "contributors" / "JacobJoergensen.html").read_text())

    def test_2171_restores_are_indexed(self):
        """The #2171 rows marked restored=yes are back in the ledger, the pages, the search index, llms-contributors.txt and the sitemaps.

        Yiming1997 keeps the discussion #2383 consent from the first restore.
        docling-project/docling-pipelines and omni-line/omni-audit already had a
        manifest for another contributor; those files stay on that contributor's pin.
        """
        audit = pathlib.Path(__file__).resolve().parents[1] / "xerj-code" / "hub" / "audits" / "2171-drops.csv"
        with audit.open(newline="") as fh:
            table = list(csv.DictReader(fh))
        self.assertEqual(len(table), 237)
        restored = [r for r in table if r["restored"] == "yes"]
        withheld = [r for r in table if r["restored"] == "no"]
        weak_restored = []
        self.assertEqual(len(restored), 200, len(restored))
        self.assertEqual(len(withheld), 37)
        authors = gen.HUB / "candidates" / "authors"
        ledger = {}
        for path in sorted(authors.glob("CONTRIBUTORS*.jsonl")):
            for line in path.read_text().splitlines():
                if line.strip():
                    row = json.loads(line)
                    ledger.setdefault(row["contributor"]["login"], []).append(row)
        shared = {
            "docling-project--docling-pipelines": "50fce561c823622482a49dfec79dbc8451388141",
            "omni-line--omni-audit": "fbe2ace2b593a6281de335e648d10fd291c78502",
        }
        for slug, sha in shared.items():
            manifest = json.loads((authors / f"{slug}.json").read_text())
            self.assertEqual(manifest["repos"][0]["sha"], sha, slug)
        people = contributors.load(gen.HUB, gen.load_registry())
        llms = (self.out / "llms-contributors.txt").read_text()
        sm_people = (self.out / "sitemap-contributors.xml").read_text()
        sm_repos = (self.out / "sitemap-repos.xml").read_text()
        sm_corpora = (self.out / "sitemap-corpora.xml").read_text()
        reg_slugs = {c["slug"] for c in gen.load_registry()}
        search = json.loads((self.out / "search" / "index.json").read_text())
        by_login = {r[0]: r for r in search["p"]}
        by_repo = {}
        for row in search["c"]:
            for repo in (row[1] or "").split():
                by_repo.setdefault(repo, row)
        for r in withheld:
            self.assertNotIn(r["login"], ledger, r["login"])
        for r in restored:
            login, pin = r["login"], r["pin"]
            # the audited corpus, found by its pin: a restored person may since have added more repos
            # (hub.xerj.org/add, e.g. QeZzhi/shark24 on 2026-10-28), which are not part of the audit
            rows = [x for x in ledger.get(login) or [] if x["pin"]["sha"] == pin]
            self.assertEqual(len(rows), 1, login)
            row = rows[0]
            lic = row["licence"]
            self.assertTrue(lic["reviewed"], login)
            self.assertEqual(lic["at_pin"], lic["spdx"], login)
            self.assertNotIn(lic["spdx"], ("", "NONE", "NOASSERTION"), login)
            self.assertEqual(row["pin"]["sha"], pin, login)
            consent = row["consent"]
            if login == "Yiming1997":
                self.assertIn("discussioncomment-19172478", consent["channel"])
            else:
                self.assertEqual(consent["channel"], "email reply to Corpus Hub outreach", login)
                self.assertNotIn("receipt", consent, login)
            slug = row["corpus"]
            self.assertIn(login, people, login)
            work = next(w for w in people[login]["work"].values() if w["pin"] == pin)
            self.assertTrue(work["reviewed"], login)
            self.assertEqual(work["spdx"], lic["spdx"], login)
            person = (self.out / "contributors" / f"{login}.html").read_text()
            corpus = (self.out / "corpus" / f"{slug}.html").read_text()
            self.assertIn(f'href="https://hub.xerj.org/contributors/{login}"', person)
            self.assertIn(f'href="../corpus/{slug}"', person)
            self.assertIn(f"agreed {consent['at'][:10]}", person)
            if row["contribution"].get("commit"):
                self.assertIn(pin, person, login)
            else:
                # The pre-drop row pinned a branch head because the selected
                # commit was not found. The corpus page still carries that pin.
                self.assertIn(pin, corpus, login)
            self.assertIn(f'href="../contributors/{login}"', corpus)
            loc = f"https://hub.xerj.org/corpus/{slug}"
            # A slug that is already a live registry corpus is listed with the
            # corpora sitemap. Every other restored corpus is a repo page, and a
            # repo page that falls under the weak threshold (repo_pages.weak) is
            # noindex,follow and left out of the sitemap like any other.
            if 'content="noindex,follow"' in corpus.split("</head>", 1)[0]:
                self.assertNotIn(loc, sm_repos, slug)
                weak_restored.append(slug)
            else:
                self.assertTrue(loc in sm_repos or loc in sm_corpora, slug)
            # Thin contributor pages are noindex and stay out of the sitemap.
            # The corpus page above is the indexable URL for that work.
            if "noindex" not in person.split("</head>", 1)[0]:
                self.assertIn(f"https://hub.xerj.org/contributors/{login}", sm_people, login)
            self.assertIn(f"(@{login})", llms)
            self.assertIn(f"https://hub.xerj.org/contributors/{login}", llms)
            self.assertIn(login, by_login, login)
            repo_name = row["repo"]
            hit = by_repo.get(repo_name)
            self.assertIsNotNone(hit, repo_name)
            self.assertEqual(hit[4], "live" if slug in reg_slugs else "review", repo_name)
            if login != "Yiming1997" and slug not in shared and slug not in reg_slugs:
                manifest = json.loads((authors / f"{slug}.json").read_text())
                src = manifest["repos"][0]
                self.assertEqual(src["sha"], pin, slug)
                self.assertEqual(src["url"], row["repo_url"], slug)
                self.assertEqual(src["review"]["use"], lic["suggested_use"], slug)
                self.assertEqual(src["review"]["by"], "author-consent", slug)
                self.assertEqual(src["review"]["at"], consent["at"], slug)
                self.assertEqual(src["review"]["spdx"], lic["at_pin"], slug)
                self.assertNotIn("consent_receipt", src["review"], slug)
                self.assertNotIn("G4", src["review"]["note"], slug)
                self.assertIn("agreed by email", corpus, slug)
                self.assertIn(consent["at"][:10], corpus, slug)
        # The weak rule is a backstop for small repos, not a way to drop the restores.
        self.assertLess(len(weak_restored), len(restored) // 2, weak_restored)

    def test_issue_2310_corpora_link_contributors_and_carry_seo(self):
        """r2r-mcp, r2r-enforce and admission-compare (Thneoly/r2r-jev, #2310)
        are live registry pages: catalogue fields, contributor links both ways,
        and the same SEO block every other corpus page gets."""
        slugs = ("r2r-mcp", "r2r-enforce", "admission-compare")
        reg = {c["slug"]: c for c in gen.load_registry()}
        llms = (self.out / "llms.txt").read_text()
        home = (self.out / "index.html").read_text()
        cat = (self.out / "category" / "AI.html").read_text()
        search = json.loads((self.out / "search" / "index.json").read_text())
        by_slug = {}
        for row in search["c"]:
            by_slug.setdefault(row[0], row)
        person = (self.out / "contributors" / "Thneoly.html").read_text()
        pins = {
            "r2r-mcp": "244ac9520db91c822a26f385a6b3bc6d4fa426a0",
            "r2r-enforce": "10c2f8ff2fe0e180227428a8f5b45ba7847d8d84",
            "admission-compare": "7a439382d306ac8599b9cf613e967bf54c86d0f2",
        }
        for slug in slugs:
            c = reg[slug]
            self.assertEqual(c["status"], "live", slug)
            self.assertEqual(c["repos"][0]["url"], "https://github.com/Thneoly/r2r-jev")
            self.assertEqual(c["repos"][0]["sha"], pins[slug])
            self.assertEqual(c["repos"][0]["review"]["spdx"], "Apache-2.0")
            self.assertEqual(c["repos"][0]["review"]["use"], "adapt-with-attribution")
            self.assertTrue(c["usecase"].strip(), slug)
            h, t, d = self._meta(f"corpus/{slug}.html")
            self.assertLessEqual(len(t), 62, t)
            self.assertGreaterEqual(len(d), 40, d)
            self.assertIn(f'href="https://hub.xerj.org/corpus/{slug}"', h)
            self.assertIn('property="og:image" content="https://hub.xerj.org/assets/og.png"', h)
            self.assertIn('name="twitter:card" content="summary_large_image"', h)
            types = set()
            for b in re.findall(r'<script type="application/ld\+json">\n(.*?)\n</script>', h, re.S):
                def walk(x):
                    if isinstance(x, dict):
                        if "@type" in x:
                            types.add(x["@type"])
                        for v in x.values():
                            walk(v)
                    elif isinstance(x, list):
                        for v in x:
                            walk(v)
                walk(json.loads(b))
            self.assertTrue({"Dataset", "SoftwareSourceCode", "BreadcrumbList"} <= types, types)
            self.assertIn('href="../contributors/Thneoly"', h)
            self.assertIn(f'href="../corpus/{slug}"', person)
            self.assertIn(f"[{slug}](https://hub.xerj.org/corpus/{slug})", llms)
            self.assertIn(f'href="corpus/{slug}"', home)
            self.assertIn(f'href="../corpus/{slug}"', cat)
            self.assertEqual(by_slug[slug][1], "Thneoly/r2r-jev")
            self.assertEqual(by_slug[slug][4], "live")
            self.assertIn(f"https://hub.xerj.org/corpus/{slug}", (self.out / "sitemap-corpora.xml").read_text())
        self.assertIn('"@type": "ProfilePage"', person)
        self.assertIn('"@type": "Person"', person)
        self.assertIn('href="https://hub.xerj.org/contributors/Thneoly"', person)

    def test_registry_page_title_names_the_repo_and_has_crumbs(self):
        h, t, d = self._meta("corpus/12-factor-agents.html")
        self.assertTrue(t.startswith("humanlayer/12-factor-agents: "), t)
        self.assertTrue(d.startswith("12-factor-agents: "), d)
        self.assertIn('aria-label="Breadcrumb"', h)
        self.assertIn('"BreadcrumbList"', h)

    def test_me_is_noindex_and_not_in_any_sitemap(self):
        self.assertIn('<meta name="robots" content="noindex', (self.out / "me.html").read_text())
        self.assertIn("/me\n  X-Robots-Tag: noindex", (self.out / "_headers").read_text())
        for f in self.out.glob("sitemap-*.xml"):
            self.assertNotIn("https://hub.xerj.org/me<", f.read_text())

    def test_home_description_ends_with_a_period_and_nothing_else_was_rewritten(self):
        html = (self.out / "index.html").read_text()
        self.assertIn(f'<meta name="description" content="{DiscoverabilityTest.HOME_DESC}">', html)
        self.assertIn(f'<meta property="og:description" content="{DiscoverabilityTest.HOME_DESC}">', html)

    def test_no_internal_html_links(self):
        bad = []
        for f in self.out.rglob("*.html"):
            for h in self.HREF.findall(f.read_text()):
                u = urllib.parse.urlsplit(h)
                if u.scheme in ("http", "https") and u.netloc != "hub.xerj.org":
                    continue
                if u.path.endswith(".html"):
                    bad.append((str(f.relative_to(self.out)), h))
        self.assertEqual(bad, [])

    def test_robots_favicon_and_card_are_written(self):
        self.assertEqual((self.out / "robots.txt").read_text(), gen.ROBOTS)
        self.assertEqual(_ihdr(self.out / "favicon.png"), (128, 128))
        self.assertEqual(_ihdr(self.out / "assets" / "og.png"), (gen.OG_W, gen.OG_H))
        self.assertIn("/favicon.ico /favicon.png 200", (self.out / "_redirects").read_text())


class XPostsTest(unittest.TestCase):
    def test_quote_is_plain_html_with_exact_text(self):
        import contributors
        post = {"id": "1", "url": "https://x.com/ann/status/1", "author_username": "ann", "author_name": "Ann <A>",
                "created_at": "2026-10-22T20:28:03.000Z", "text": "a &amp; b &gt; c \u2014 \"q\"\nnext",
                "match": "post links /contributors/ann"}
        h = contributors.xposts_section([post], gen.esc)
        self.assertIn("<h2>On X</h2>", h)
        self.assertIn("<p>a &amp; b &gt; c \u2014 &quot;q&quot;<br>next</p>", h)
        self.assertIn("Ann &lt;A&gt; (@ann)", h)
        self.assertIn('<time datetime="2026-10-22T20:28:03.000Z">Oct 22, 2026</time>', h)
        self.assertIn('href="https://x.com/ann/status/1" rel="nofollow noopener"', h)
        self.assertNotIn("<script", h)
        self.assertEqual(contributors.xposts_section([], gen.esc), "")

    def test_data_file_shape(self):
        p = pathlib.Path(__file__).resolve().parent / "data" / "x_posts.json"
        if not p.exists():
            self.skipTest("no data/x_posts.json")
        data = json.loads(p.read_text(encoding="utf-8"))
        for login, posts in data.items():
            for x in posts:
                self.assertTrue({"id", "url", "author_username", "author_name", "created_at", "text"} <= set(x), login)
                self.assertTrue(x["url"].startswith("https://x.com/"), login)


class HubIdentityTest(unittest.TestCase):
    """Issue #2179: a listing has to say which upstream project a slug is.

    `fabric` is danielmiessler/fabric, not fabric.js. The card, the detail
    source link, the paste prompt and the llms.txt line all carry owner/name
    plus the one-line description.
    """

    FABRIC = {
        "slug": "fabric", "lane": "A", "cat": "AI", "status": "live", "refresh": "pinned",
        "usecase": "An agent building a reusable prompt library queries this for prompt patterns.",
        "repos": [{
            "repo": "fabric", "url": "https://github.com/danielmiessler/fabric.git",
            "sha": "b" * 40, "bytes": 32_000_000, "files": 400,
            "review": {"spdx": "MIT", "use": "adapt-with-attribution", "note": "ok"},
        }],
        "added": "2026-10-01T00:00:00Z", "kind": "reference corpus", "consumed": "",
        "tags": {"region": "global", "topics": ["prompts"]}, "merged_into": "",
    }

    def test_upstream_label_is_owner_and_repo_not_the_bare_name(self):
        self.assertEqual(gen.upstream_label(self.FABRIC), "danielmiessler/fabric")
        self.assertEqual(gen.upstream_label(LANE_A), "lz4/lz4")
        bare = {"repos": [{"repo": "fabric", "url": ""}], "sources": []}
        self.assertEqual(gen.upstream_label(bare), "")
        ssh = {"repos": [{"repo": "WebGL", "url": "git@github.com:KhronosGroup/WebGL.git"}], "sources": []}
        self.assertEqual(gen.upstream_label(ssh), "KhronosGroup/WebGL")

    def test_card_names_the_upstream_repo_and_the_one_line(self):
        html = gen.card(self.FABRIC, {}, {}, {})
        self.assertIn('class="crepo">danielmiessler/fabric</p>', html)
        self.assertIn("prompt library", html)
        self.assertIn('class="cname">fabric</span>', html)

    def test_detail_source_link_is_owner_and_repo(self):
        body = gen.detail_body(self.FABRIC, {}, {}, {}, {})
        self.assertIn(">danielmiessler/fabric</a>", body)
        self.assertIn('class="crepo">danielmiessler/fabric</p>', body)
        self.assertIn("prompt library", body)
        self.assertNotIn(">fabric</a>", body)

    def test_prompt_names_the_upstream_repo(self):
        p = gen.install_prompt(self.FABRIC)
        self.assertIn("Upstream repo: danielmiessler/fabric.", p)
        self.assertIn("prompt library", p)
        self.assertNotIn("\u2014", p)
        self.assertNotIn("\u2013", p)

    def test_tile_title_and_label_name_the_upstream_repo(self):
        html = gen.tile(self.FABRIC)
        self.assertIn("danielmiessler/fabric", html)
        self.assertIn('title="fabric · danielmiessler/fabric', html)


class SearchIdentityTest(unittest.TestCase):
    """The static search box must show owner/name and the one-line description,
    and a same-stem different project (`fabric.js`) must not look like a hit
    on the slug."""

    def test_index_row_stores_the_github_owner_and_repo(self):
        corpora = [HubIdentityTest.FABRIC]
        idx = search_index.build(corpora, {}, {}, {}, {}, gen.plain, "2026-10-23")
        row = idx["c"][0]
        self.assertEqual(row[0], "fabric")
        self.assertEqual(row[1], "danielmiessler/fabric")
        self.assertIn("prompt library", row[7])

    def test_browser_search_labels_a_different_project_of_the_same_stem(self):
        import subprocess
        import tempfile
        js_path = pathlib.Path(tempfile.mkdtemp()) / "search.js"
        js_path.write_text(search_index.JS, encoding="utf-8")
        harness = r"""
const fs = require('fs');
const vm = require('vm');
const js = fs.readFileSync(process.argv[1], 'utf8');
const window = {};
const document = {
  currentScript: { src: 'https://hub.xerj.org/assets/search.js' },
  querySelectorAll() { return []; },
  getElementById() { return null; },
  addEventListener() {},
};
const context = {
  window, document, performance: { now: () => 1 },
  fetch: () => Promise.reject(new Error('offline')),
  URL, URLSearchParams, console,
  history: { replaceState() {} },
  location: { href: 'https://hub.xerj.org/search/', search: '' },
};
vm.createContext(context);
vm.runInContext(js, context);
const T = window.__hubSearch;
if (T.projectStem('fabric.js') !== 'fabric') { console.error('stem', T.projectStem('fabric.js')); process.exit(2); }
if (T.projectStem('lz4.h') !== '') { console.error('header', T.projectStem('lz4.h')); process.exit(3); }
if (T.projectStem('fabricjs') !== 'fabric') { console.error('glued', T.projectStem('fabricjs')); process.exit(4); }
const fabric = {t:'c', slug:'fabric', repos:['danielmiessler/fabric'], stars:0, dom:'AI', st:'live',
  k:['fabric','danielmiessler/fabric','fabric','danielmiessler'], w:['prompts'],
  use:'An agent building a reusable prompt library', u:['an','agent','building','reusable','prompt','library']};
const candle = {t:'c', slug:'candle', repos:['huggingface/candle'], stars:0, dom:'ML', st:'live',
  k:['candle','huggingface/candle','candle','huggingface'], w:['rust'],
  use:'A Rust tensor framework', u:['rust','tensor','framework']};
T._set([fabric, candle], []);
const hit = T.search('fabric.js', 5);
if (!hit || !hit.c.n || hit.c.items[0].slug !== 'fabric') { console.error('no fabric hit', JSON.stringify(hit)); process.exit(5); }
const html = T.row(hit.c.items[0], 'fabric.js', 'r0');
for (const need of ['not fabric.js', 'danielmiessler/fabric', 'prompt library']) {
  if (!html.includes(need)) { console.error('missing', need, html); process.exit(6); }
}
const exact = T.row(fabric, 'fabric', 'r1');
if (exact.includes('not fabric')) { console.error('exact slug labeled different', exact); process.exit(7); }
if (!exact.includes('danielmiessler/fabric') || !exact.includes('prompt library')) { console.error('exact missing identity', exact); process.exit(8); }
const charts = T.row(candle, 'candle charts', 'r2');
if (charts.includes('not candle')) { console.error('candle charts is not a different project', charts); process.exit(9); }
if (!charts.includes('huggingface/candle') || !charts.includes('tensor framework')) { console.error('candle identity', charts); process.exit(10); }
const named = T.collisionToken('see fabric.js', Object.assign({}, fabric, {use:'the fabric.js canvas library'}));
if (named) { console.error('token already in the description', named); process.exit(11); }
console.log('ok');
"""
        proc = subprocess.run(
            ["node", "-e", harness, str(js_path)],
            check=False, capture_output=True, text=True,
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertIn("ok", proc.stdout)



class GhStarsBadgeTest(unittest.TestCase):
    """The nav GitHub stars badge (gh_stars.py): a real saved count, one per page,
    refreshed straight from api.github.com so it never adds hub Worker requests."""

    def test_compact(self):
        for n, want in ((950, "950"), (1000, "1k"), (1249, "1.2k"), (1250, "1.3k"), (14225, "14.2k"), (15049, "15k")):
            self.assertEqual(gh_stars.compact(n), want)

    def test_badge_on_every_page_with_saved_count(self):
        n = gh_stars.saved_count()
        self.assertIsInstance(n, int)
        self.assertGreater(n, 0)
        out = gen.page("T", "D", "<main></main>")
        self.assertEqual(out.count('class="navstars"'), 1)
        self.assertIn('href="https://github.com/xerj-org/xerj"', out)
        self.assertIn(f'data-gh-stars="{n}">\u2605 {gh_stars.compact(n)}</span>', out)
        self.assertIn(gh_stars.JS, out)
        self.assertNotRegex(gh_stars.badge() + gh_stars.JS, "[\u2013\u2014]")

    def test_refresh_goes_to_github_not_the_worker(self):
        urls = re.findall(r"fetch\('([^']+)'", gh_stars.JS)
        self.assertEqual(urls, ["https://api.github.com/repos/xerj-org/xerj"])
        self.assertIn("600000", gh_stars.JS)  # 10-minute localStorage cache

    def test_missing_saved_count_still_renders_the_mark(self):
        with tempfile.TemporaryDirectory() as d:
            old = gh_stars.SAVED
            gh_stars.SAVED = pathlib.Path(d) / "none.json"
            try:
                b = gh_stars.badge()
            finally:
                gh_stars.SAVED = old
        self.assertIn('data-gh-stars="0"></span>', b)
        self.assertIn("<svg", b)

if __name__ == "__main__":
    unittest.main()
