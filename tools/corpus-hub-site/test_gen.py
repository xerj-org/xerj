#!/usr/bin/env python3
"""Regression tests for gen.py (the Corpus Hub site generator).

Stdlib only, no network, deterministic — same constraints as gen.py. Run:

    python3 tools/corpus-hub-site/test_gen.py

Covers issue #1279: a graded backlog file whose "graded" value is a string
(a timestamp note, not a slug->suite map) must not crash the whole site
build. One malformed file froze every corpus discovery page in the deploy.
"""
import contextlib
import io
import json
import os
import pathlib
import re
import sys
import urllib.parse
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
        idx = contributors.by_corpus(people)
        self.assertEqual(idx["themed"], ["Ann", "cat"])
        self.assertEqual(idx["live-one"], ["Ann"])
        html = contributors.corpus_section(idx["themed"], gen.esc, people)
        self.assertIn('href="../contributors/Ann.html"', html)
        self.assertEqual(contributors.corpus_section([], gen.esc, people), "")

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
    # issue #1891: step 2 must cover the busy-port case and say which port --url takes.
    def test_setup_steps_name_the_port_flag_and_the_url_port(self):
        self.assertIn("--port 9400", gen.SETUP_STEPS)
        self.assertIn("--url http://127.0.0.1:9400", gen.SETUP_STEPS)
        self.assertIn("ES-compat", gen.SETUP_STEPS)

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
            people = [h for h in hrefs if "contributors/" in h and not h.endswith("contributors/index.html")]
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
        first = re.findall(r'<td class="who"><a href="([^"]+)\.html">', people)
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
        self.assertIn(f"<b>{contributors.compact(stars)}</b><span>GitHub stars across pinned repos</span>", strip)
        self.assertIn(f"{stars:,} GitHub stars, summed over {n:,} distinct pinned repositories", strip)

    def test_whats_this_replaces_about(self):
        self.assertTrue((self.out / "whats-this" / "index.html").is_file())
        self.assertFalse((self.out / "about.html").exists())
        self.assertIn("/about.html /whats-this/ 301", (self.out / "_redirects").read_text())

    def test_no_broken_relative_links_anywhere(self):
        bad = []
        for f in self.out.rglob("*.html"):
            bad += [(str(f.relative_to(self.out)), h) for h in self.HREF.findall(f.read_text()) if not self._resolves(f, h)]
        self.assertEqual(bad, [])


if __name__ == "__main__":
    unittest.main()
