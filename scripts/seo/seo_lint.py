#!/usr/bin/env python3
"""CI gate for xerj.org's technical SEO invariants.

Implements the technical-SEO invariants that can be checked against the static
tree alone: the canonical/Open Graph/structured-data set, sitemap coverage and
the extensionless URL shape.  They follow the sitemaps.org protocol and Google
Search Central's crawling-and-indexing guidance; each rule below carries its own
number and its own explanation.  Rules 15, 22 and 63 need the live site (a
sitemap `<loc>` has to return 200) and are not implemented here — they belong in
a post-deploy job, not the merge gate.  They now exist, in
`scripts/seo/verify_live.py`; run it after a deploy, never in CI.

Rule 67 is the boundary case between the two.  It cannot see production either,
but it does not need to: Cloudflare Pages derives its not-found behaviour from
the published tree, so "does this site return a real 404" is answerable from
the committed files alone — and until 2026-08-22 the answer was no.

Exit codes
    0   no gate violations
    1   at least one gate violation (or a warning, with --warnings-as-errors)

    python3 scripts/seo/seo_lint.py
    python3 scripts/seo/seo_lint.py --warnings-as-errors

Dependencies: Python 3 stdlib only, plus `xmllint` if it happens to be on
PATH (rule 12 is skipped with a note when it is not).  The HTML parsing is
`scripts/seo/audit.py`'s `PageParser` — one parser for the whole SEO
toolchain, so a parsing bug cannot make the auditor and the gate disagree.
"""

from __future__ import annotations

import argparse
import collections
import html
import json
import pathlib
import re
import shutil
import subprocess
import sys
import urllib.parse
import xml.etree.ElementTree as ET

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import audit as audit_mod  # noqa: E402  (reuse the auditor's HTML parser)
import pagedata  # noqa: E402
import urlmap  # noqa: E402

SITEMAP_NS = "{http://www.sitemaps.org/schemas/sitemap/0.9}"

#: The exact `script-src` this site is allowed to ship.  JSON-LD is a data
#: block (WHATWG HTML "prepare the script element" returns at step 13, before
#: the CSP inline check at step 21), so structured data never needs a CSP
#: change.  Rule 55 exists to stop someone "fixing" that non-problem by
#: widening the policy.  The `'unsafe-inline'` here predates this work and is
#: something to remove later, not to build on.
ALLOWED_SCRIPT_SRC = frozenset({
    "'self'", "blob:", "'unsafe-inline'", "https://static.cloudflareinsights.com",
})

#: Internal links that are known-broken and consciously left that way.
#: Every entry needs a reason and an owner decision, not just a path.
KNOWN_BROKEN: dict[str, str] = {
    "/resources/xerj-sales-kit.zip":
        "the ZIP has never existed; the page promises '12 PDFs, ~2.4 MB' but the "
        "12 PDFs on disk total 4.4 MB, so building it would contradict visible "
        "copy this pass is not allowed to edit. Needs a human: ship a real kit "
        "and correct the size, or repoint the CTA at the individual briefs.",
}

#: Rule 64 — file types under `landing/` that are not text and are therefore
#: not scanned for stale `.html` URLs.  Everything else is scanned, including
#: files with no extension at all (`landing/get`).
_BINARY_SUFFIXES = frozenset({
    ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico", ".pdf", ".zip", ".gz",
    ".mp4", ".webm", ".woff", ".woff2", ".wasm",
})

#: Rule 64 allowlist — the only literals under `landing/` that may still spell
#: a `.html` path.  Keys are landing-relative file paths; each value maps the
#: exact allowed literal to why it must stay.  The list is deliberately keyed
#: per-file and per-literal: a new `.html` URL anywhere, including in these two
#: files, is still a gate violation.
#:
#: Both entries are Cloudflare Pages *configuration*, not published links.  A
#: `_headers` rule head and a `_redirects` comment are matched against, or
#: describe, the on-disk asset path — rewriting them extensionless would either
#: silently stop applying (`_headers`) or make the comment describe a file that
#: is not what ships.
HTML_PATH_ALLOWLIST: dict[str, dict[str, str]] = {
    "_headers": {
        "/index.html":
            "`_headers` rule head. The root payload is served at both `/` and "
            "`/index.html`; the file has a rule for each, and the asset path "
            "has to be spelled out for the second one to match.",
        "/playground/*.html":
            "`_headers` rule head — a Cloudflare glob matched against the "
            "request path for the dashboards SPA's HTML assets, not a link.",
    },
    "_redirects": {
        "/product.html":
            "comment only: the file-layout header block names the files on "
            "disk, not the URLs they are served at.",
        "/brand.html":
            "comment only: same file-layout header block.",
        "/playground.html":
            "comment only: the header block, plus the note recording why "
            "pointing an alias at `/playground.html` cost two hops. Rewriting "
            "the note extensionless would erase what it documents.",
    },
}

#: Rule 64 — a `.html` path published as an absolute URL (`xerj.org/x.html`,
#: with or without a scheme) or as a root-relative path (`/x.html`).  The
#: root-relative pattern's lookbehind keeps it from re-matching the path half
#: of a URL that the first pattern already covers, and from firing inside an
#: off-origin URL such as `https://github.com/o/r/blob/main/x.html`.
_PATH_CHARS = r"[A-Za-z0-9._~!$&'()*+,;=:@%/*-]"
STALE_URL_RE = re.compile(rf"xerj\.org(?P<path>/{_PATH_CHARS}*\.html)")
STALE_PATH_RE = re.compile(rf"(?<![A-Za-z0-9._~%:/-])(?P<path>/{_PATH_CHARS}*\.html)")

#: Rule 58/59 (every <img> has width/height/alt) is `[gate]` in the research
#: doc but is reported here as a warning: `demo/index.html` ships 12 unsized
#: screenshots plus one `<img src="">`, and adding intrinsic dimensions to a
#: page with its own layout CSS is a rendering change, not a metadata change.
#: Flip this to False once those images carry intrinsic dimensions; the rule
#: itself is already written and will start gating with no other edit.
IMG_RULES_ARE_WARNINGS = True

TITLE_MAX = 60
DESC_MIN, DESC_MAX = 110, 165
MAX_DEPTH = 3

GENERIC_ANCHORS = {"click here", "here", "read more", "this", "link", "more"}


#: Rule 65 — a GitHub ref that cannot move.  ``main``/``master`` are branches
#: too, but they are the repository's permanent trunk and survive a merge; a
#: tag and a commit SHA are immutable.  Anything else — ``feat/agentic-seo``,
#: ``fix/x``, a personal branch — is deleted after merge and the link 404s.
_PERMANENT_GITHUB_REF_RE = re.compile(
    r"^(?:main|master|HEAD"
    r"|[0-9a-f]{7,40}"
    r"|v?\d+(?:\.\d+)*(?:[-.][0-9A-Za-z.]+)*)$")

#: Rule 65 — GitHub URL shapes that carry a ref as their next path segment.
#: ``feat/agentic-seo`` captures as ``feat``, which is not a permanent ref, so
#: a branch-pinned URL trips the rule whether or not the branch name has a
#: slash in it.
_GITHUB_REF_URL_RE = re.compile(
    r"https?://(?:"
    r"github\.com/[\w.-]+/[\w.-]+/(?:blob|tree|raw|blame|edit|commits)/"
    r"|raw\.githubusercontent\.com/[\w.-]+/[\w.-]+/"
    r")(?P<ref>[^/\s\"'`)<>\]]+)")

#: Rule 66 — text inside an ``<em>``/``<strong>`` that is really a chopped-up
#: identifier.  ``ignored_files=2, ignored_dirs=3`` used to render as
#: ``ignored<em>files=2, ignored</em>dirs=3`` because the generator applied
#: intraword ``_`` emphasis.  The generator no longer does; this rule is the
#: backstop, and it keys on the giveaway that no prose italic has: a word
#: character welded to the tag on either side.
_INTRAWORD_EMPHASIS_RE = re.compile(
    r"(?:\w<(?:em|strong)>)|(?:</(?:em|strong)>\w)")

#: Rule 67 — the file whose presence decides whether this site can return an
#: HTTP 404 at all.  Cloudflare Pages: "If your project does not include a
#: top-level ``404.html``, Pages assumes that you are deploying a single-page
#: application" and answers every unmatched path with the root ``index.html``
#: and a 200.  The path is landing-relative because that is what Pages
#: publishes verbatim.
NOT_FOUND_PAGE = "404.html"

#: Rule 67 — a `_redirects` source pattern that matches paths nothing else
#: claimed.  ``/*`` is the shape the SPA rewrite (``/* /index.html 200``)
#: uses; ``/:splat``-style placeholders at the root are the same thing spelled
#: differently.  A rule that matches everything re-creates the soft 404 that
#: `404.html` exists to remove, whatever status code it carries: with 200 it
#: is the soft 404 itself, and with a 3xx it turns every wrong URL into a
#: redirect to a real page, which Google also treats as a soft 404.
_CATCH_ALL_SOURCE_RE = re.compile(r"^/(?:\*|:[A-Za-z_][\w-]*)$")


class Issue:
    __slots__ = ("level", "rule", "path", "message")

    def __init__(self, level: str, rule: str, path: str, message: str):
        self.level, self.rule, self.path, self.message = level, rule, path, message

    def __str__(self) -> str:
        return f"[{self.level.upper():4}] {self.rule:<28} {self.path}: {self.message}"


class Lint:
    def __init__(self, root: pathlib.Path, repo: pathlib.Path):
        self.root = root
        self.repo = repo
        self.issues: list[Issue] = []
        self.pages: dict[str, audit_mod.Page] = {}
        self.raw: dict[str, str] = {}
        self._parse()

    # -- helpers ------------------------------------------------------------
    def gate(self, rule: str, path: str, msg: str) -> None:
        self.issues.append(Issue("gate", rule, path, msg))

    def warn(self, rule: str, path: str, msg: str) -> None:
        self.issues.append(Issue("warn", rule, path, msg))

    def _parse(self) -> None:
        for rel in urlmap.iter_pages(self.root):
            f = self.root / rel
            src = f.read_text(encoding="utf-8", errors="replace")
            self.raw[rel] = src
            page = audit_mod.Page(path=rel, abs_path=f)
            parser = audit_mod.PageParser(page)
            parser.feed(src)
            parser.close()
            self.pages[rel] = page

    @staticmethod
    def _meta(page: audit_mod.Page, key: str) -> str | None:
        return page.metas.get(key)

    # ── sitemap ↔ filesystem (rules 1-14) ───────────────────────────────────
    def check_sitemap(self) -> None:
        sm = self.root / "sitemap.xml"
        p = "sitemap.xml"
        if not sm.exists():
            self.gate("sitemap.missing", p, "landing/sitemap.xml does not exist")
            return

        data = sm.read_bytes()
        if data.startswith(b"\xef\xbb\xbf"):
            self.gate("sitemap.bom", p, "file starts with a UTF-8 BOM (rule 13)")
        try:
            text = data.decode("utf-8")
        except UnicodeDecodeError as e:
            self.gate("sitemap.encoding", p, f"not valid UTF-8: {e} (rule 13)")
            return
        if len(data) > 50_000_000:
            self.gate("sitemap.size", p, f"{len(data)} bytes exceeds 50 MB (rule 10)")

        try:
            tree = ET.fromstring(text)
        except ET.ParseError as e:
            self.gate("sitemap.malformed", p, f"XML does not parse: {e}")
            return
        if tree.tag != f"{SITEMAP_NS}urlset":
            self.gate("sitemap.namespace", p,
                      f"root is {tree.tag}, expected {SITEMAP_NS}urlset (rule 11)")

        if "<priority>" in text or "<changefreq>" in text:
            self.gate("sitemap.dead_hints", p,
                      "<priority>/<changefreq> present — both confirmed ignored "
                      "by Google and Bing (rule 7)")
        if re.search(r"<loc>[^<]*&(?!amp;|lt;|gt;|quot;|apos;|#)", text):
            self.gate("sitemap.unescaped", p,
                      "unescaped '&' in a <loc> — makes the file malformed (rule 13)")

        locs: list[str] = []
        today = _today()
        for url in tree.findall(f"{SITEMAP_NS}url"):
            loc_el = url.find(f"{SITEMAP_NS}loc")
            loc = (loc_el.text or "").strip() if loc_el is not None else ""
            locs.append(loc)
            if urllib.parse.urlsplit(loc).path.lower().endswith(".md"):
                self.gate("sitemap.markdown", p,
                          f"{loc} is a Markdown twin; twins must stay out of sitemap.xml")
                continue
            if not loc.startswith(urlmap.SITE_ORIGIN + "/") and loc != urlmap.SITE_ORIGIN + "/":
                self.gate("sitemap.not_absolute", p,
                          f"{loc!r} is not an absolute https://xerj.org URL (rule 5)")
                continue
            loc_path = urllib.parse.urlsplit(loc).path.rstrip("/")
            if loc.endswith(".html") or loc_path == "/index" or loc_path.endswith("/index"):
                self.gate("sitemap.html_form", p,
                          f"{loc} uses a redirecting URL form (rule 6)")
            rel = urlmap.rel_for_url(loc)
            if rel is None or not (self.root / rel).is_file():
                self.gate("sitemap.dead_url", p,
                          f"{loc} corresponds to no file on disk (rule 2)")
            elif not rel.endswith(".html"):
                self.gate("sitemap.non_html", p,
                          f"{loc} is not an HTML page (rule 14)")
            lm_el = url.find(f"{SITEMAP_NS}lastmod")
            if lm_el is not None:
                lm = (lm_el.text or "").strip()
                if not re.fullmatch(r"\d{4}-\d{2}-\d{2}(T.*)?", lm):
                    self.gate("sitemap.lastmod_format", p,
                              f"{loc}: lastmod {lm!r} is not W3C Datetime (rule 8)")
                elif lm[:10] > today:
                    self.gate("sitemap.lastmod_future", p,
                              f"{loc}: lastmod {lm} is in the future (rule 9)")

        if len(locs) > 50_000:
            self.gate("sitemap.count", p, f"{len(locs)} URLs exceeds 50,000 (rule 10)")
        dupes = [u for u, n in collections.Counter(locs).items() if n > 1]
        for u in dupes:
            self.gate("sitemap.duplicate", p, f"{u} appears more than once (rule 3)")

        expected = {urlmap.deployed_url(rel) for rel in self.pages
                    if urlmap.in_sitemap(rel)}
        for missing in sorted(expected - set(locs)):
            self.gate("sitemap.missing_page", p,
                      f"{missing} is an indexable page but is not listed (rule 1)")

        # rule 4 — the file must be generated, not hand-edited
        import gen_sitemap
        dates = urlmap.DateSource(self.repo)
        if text != gen_sitemap.build(self.root, dates):
            self.gate("sitemap.stale", p,
                      "differs from `gen_sitemap.py --write` output — it was "
                      "hand-edited or is out of date (rule 4)")

        # rule 12 — XSD validation, when xmllint is available
        xsd = self.repo / "schemas" / "sitemap.xsd"
        if shutil.which("xmllint") and xsd.exists():
            r = subprocess.run(["xmllint", "--noout", "--schema", str(xsd), str(sm)],
                               capture_output=True, text=True)
            if r.returncode != 0:
                self.gate("sitemap.xsd", p, f"xmllint: {r.stderr.strip()} (rule 12)")

    # ── robots.txt (rules 38, 39) ───────────────────────────────────────────
    def check_robots(self) -> None:
        f = self.root / "robots.txt"
        p = "robots.txt"
        if not f.exists():
            self.gate("robots.missing", p, "landing/robots.txt does not exist")
            return
        text = f.read_text(encoding="utf-8")
        if f"Sitemap: {urlmap.SITE_ORIGIN}/sitemap.xml" not in text:
            self.gate("robots.sitemap", p,
                      f"no `Sitemap: {urlmap.SITE_ORIGIN}/sitemap.xml` line (rule 39)")
        disallows = [m.group(1).strip()
                     for m in re.finditer(r"^\s*Disallow:\s*(\S+)", text, re.M)]
        for rel in urlmap.NOINDEX:
            path = urlmap.deployed_path(rel)
            for d in disallows:
                if d and path.startswith(d):
                    self.gate("robots.noindex_blocked", p,
                              f"Disallow: {d} hides {path}, whose noindex would "
                              f"then never be seen (rule 38)")

    # ── CSP (rule 55) ───────────────────────────────────────────────────────
    def check_headers(self) -> None:
        f = self.root / "_headers"
        p = "_headers"
        if not f.exists():
            self.warn("headers.missing", p, "landing/_headers does not exist")
            return
        for m in re.finditer(r"script-src([^;]*)", f.read_text(encoding="utf-8")):
            for src in m.group(1).split():
                if src not in ALLOWED_SCRIPT_SRC:
                    self.gate("csp.script_src_widened", p,
                              f"script-src gained {src!r}. JSON-LD is a data "
                              f"block and never needs a CSP change (rule 55)")

    # ── stale .html URLs in any published file (rule 64) ────────────────────
    def check_published_urls(self) -> None:
        """No file Cloudflare publishes may spell a `.html` URL.

        Rule 21 covers `<a href>` in HTML.  It cannot see the rest of what
        ships: `llms.txt` and `llms-full.txt` are the agent-facing entry points
        to the site and are pure text, and an absolute `https://xerj.org/x.html`
        is not an "internal href" at all, so it slipped past the site-wide
        migration in two print-only footers.  This rule reads every published
        text file byte-for-byte, so there is nowhere left for a redirecting URL
        to hide.

        Overlap with rule 21 on root-relative hrefs inside HTML is deliberate:
        one rule reasons about parsed links, this one about published bytes,
        and a regression should trip both rather than neither.
        """
        for f in sorted(self.root.rglob("*")):
            if not f.is_file() or f.suffix.lower() in _BINARY_SUFFIXES:
                continue
            rel = f.relative_to(self.root).as_posix()
            try:
                text = f.read_text(encoding="utf-8")
            except (UnicodeDecodeError, OSError):
                continue  # not a text file we can reason about
            allowed = HTML_PATH_ALLOWLIST.get(rel, {})
            for regex, kind in ((STALE_URL_RE, "absolute URL"),
                                (STALE_PATH_RE, "root-relative path")):
                for m in regex.finditer(text):
                    path = m.group("path")
                    if path in allowed:
                        continue
                    line = text.count("\n", 0, m.start()) + 1
                    served = self._served_form(path)
                    self.gate("url.html_form", rel,
                              f"line {line}: {kind} {m.group(0)!r} ends in "
                              f"`.html` — Cloudflare 308s it to {served} "
                              f"(rule 64)")

    @staticmethod
    def _served_form(path: str) -> str:
        """The URL Cloudflare actually serves for a `.html` path, for the
        message text.  Falls back to the literal when the path is not a page
        (a glob, say), so the report never invents a target."""
        target = urlmap.rel_for_path(path)
        if target is None or "*" in path:
            return path
        try:
            return urlmap.deployed_path(target)
        except ValueError:
            return path

    # ── per-page head, canonical, structured data ───────────────────────────
    # ── branch-pinned GitHub URLs (rule 65) ─────────────────────────────────
    def check_pinned_refs(self) -> None:
        """No published or authored file may cite a mutable GitHub branch.

        Three writing agents emitted evidence sources as
        ``https://github.com/xerj-org/xerj/blob/feat/agentic-seo/<path>`` while
        working around a build bug.  That branch is deleted on merge, so every
        one of those links dies the day the PR lands.  A repo-relative path is
        the correct form for in-repo evidence; when a URL really is wanted it
        has to name a ref that survives — ``main``, a tag, or a commit SHA.

        Both trees are scanned.  ``content/`` is where the mistake is authored
        and ``landing/`` is where it ships, and a regression should be caught
        at the source rather than only in the generated output.
        """
        for label, base in (("content", self.repo / "content"),
                            ("landing", self.root)):
            if not base.is_dir():
                continue
            for f in sorted(base.rglob("*")):
                if not f.is_file() or f.suffix.lower() in _BINARY_SUFFIXES:
                    continue
                try:
                    text = f.read_text(encoding="utf-8")
                except (UnicodeDecodeError, OSError):
                    continue
                rel = f.relative_to(base).as_posix()
                if label == "content":
                    rel = f"../content/{rel}"
                for m in _GITHUB_REF_URL_RE.finditer(text):
                    ref = m.group("ref")
                    if _PERMANENT_GITHUB_REF_RE.fullmatch(ref):
                        continue
                    line = text.count("\n", 0, m.start()) + 1
                    self.gate("url.branch_pinned", rel,
                              f"line {line}: {m.group(0)!r} pins a mutable "
                              f"GitHub ref {ref!r}. The branch is deleted "
                              f"after merge and the link 404s — use a "
                              f"repo-relative path, or blob/main, a tag or a "
                              f"commit SHA (rule 65)")

    # ── identifiers italicised by intraword `_` (rule 66) ───────────────────
    def check_intraword_emphasis(self) -> None:
        """No ``<em>``/``<strong>`` may be welded to a surrounding word.

        Genuine prose italics always sit at a word boundary.  An emphasis tag
        with a word character immediately against it is the signature of an
        identifier that was split by intraword ``_`` emphasis, which is a
        rendering bug rather than a writing choice.
        """
        for f in sorted(self.root.rglob("*")):
            if not f.is_file() or f.suffix.lower() not in (".html", ".md"):
                continue
            try:
                text = f.read_text(encoding="utf-8")
            except (UnicodeDecodeError, OSError):
                continue
            rel = f.relative_to(self.root).as_posix()
            for m in _INTRAWORD_EMPHASIS_RE.finditer(text):
                line = text.count("\n", 0, m.start()) + 1
                snippet = text[max(0, m.start() - 40):m.end() + 40].replace("\n", " ")
                self.gate("emphasis.intraword", rel,
                          f"line {line}: {m.group(0)!r} — an emphasis tag is "
                          f"welded to a word character, which means an "
                          f"identifier was italicised by intraword `_` "
                          f"emphasis: {snippet!r} (rule 66)")

    # ── the site can return a real 404 (rule 67) ────────────────────────────
    def check_not_found(self) -> None:
        """`landing/404.html` must exist and nothing may swallow unknown paths.

        Cloudflare Pages publishes `landing/` verbatim and decides its
        not-found behaviour from the tree alone.  With no top-level
        `404.html` it treats the project as a single-page application and
        answers **every** unmatched path with the root `index.html` and an
        HTTP **200**.  That is a soft 404: a typo'd URL, a dead link and a
        URL an AI crawler guessed all return a plausible page instead of an
        error, every one of them a byte-identical duplicate of the homepage,
        and a removed page can never be signalled as removed.  Verified on
        the live site on 2026-08-22, before this file existed:

            /                                200  21841 B  ETag 93477f59…
            /this-page-has-never-existed…    200  21841 B  ETag 93477f59…
            /answers/what-is-xerj            200  21841 B  ETag 93477f59…

        Three URLs, one of which has never existed, one body, one ETag.

        The second half of the rule is the way the same failure comes back
        after `404.html` is added: one catch-all line in `_redirects`.
        `/* /index.html 200` restores the soft 404 exactly, and a catch-all
        with a 3xx status is the redirect-shaped variant Google also files as
        a soft 404.  Named prefixes (`/docs/*`) are fine — they claim a real
        section; only a source that matches every path is a violation.
        """
        page = self.root / NOT_FOUND_PAGE
        if not page.is_file():
            self.gate("notfound.missing", NOT_FOUND_PAGE,
                      "the site has no top-level 404.html, so Cloudflare Pages "
                      "serves index.html with HTTP 200 for every unknown path — "
                      "a sitewide soft 404 (rule 67)")

        redirects = self.root / "_redirects"
        if not redirects.is_file():
            return
        for n, raw in enumerate(redirects.read_text(encoding="utf-8").splitlines(), 1):
            line = raw.split("#", 1)[0].strip()
            if not line:
                continue
            source = line.split()[0]
            if _CATCH_ALL_SOURCE_RE.fullmatch(source):
                self.gate("redirects.catch_all", "_redirects",
                          f"line {n}: {line!r} — a catch-all source matches every "
                          f"path nothing else claimed, so unknown URLs never reach "
                          f"404.html and the site soft-404s again (rule 67)")

    def check_pages(self) -> None:
        titles: dict[str, list[str]] = collections.defaultdict(list)
        descs: dict[str, list[str]] = collections.defaultdict(list)

        for rel, page in self.pages.items():
            src = self.raw[rel]
            url = urlmap.deployed_url(rel)
            head = _head_of(src)

            # -- canonical (16-19)
            if len(page.canonicals) == 0:
                self.gate("canonical.missing", rel, "no <link rel=canonical> (rule 16)")
            elif len(page.canonicals) > 1:
                self.gate("canonical.conflict", rel,
                          f"{len(page.canonicals)} canonicals (rules 16, 35)")
            else:
                href = page.canonicals[0][0].strip()
                if f'rel="canonical" href="{href}"' not in head and href not in head:
                    self.gate("canonical.not_in_head", rel,
                              "canonical is outside <head>, so Google ignores it (rule 16)")
                if href != url:
                    self.gate("canonical.mismatch", rel,
                              f"canonical {href} != deployed URL {url} (rule 17)")
                if href.endswith(".html"):
                    self.gate("canonical.html", rel,
                              f"canonical {href} ends in .html and 308-redirects (rule 19)")
                if "#" in href or "?" in href or not href.startswith("https://"):
                    self.gate("canonical.form", rel,
                              f"canonical {href} must be absolute https with no "
                              f"fragment or query (rule 18)")

            # -- title (23, 24, 25, 35)
            title = (page.title or "").strip()
            if not title:
                self.gate("title.missing", rel, "empty or absent <title> (rule 23)")
            else:
                titles[title].append(rel)
                if len(title) > TITLE_MAX:
                    self.warn("title.long", rel,
                              f"<title> is {len(title)} chars (>{TITLE_MAX}) (rule 25)")
            if page.titles_seen > 1:
                self.gate("title.multiple", rel,
                          f"{page.titles_seen} <title> elements (rule 35)")

            # -- description (26, 27, 28, 35)
            desc = (page.description or "").strip()
            if not desc:
                self.gate("desc.missing", rel, "no meta description (rule 26)")
            else:
                descs[desc].append(rel)
                if not (DESC_MIN <= len(desc) <= DESC_MAX):
                    self.warn("desc.size", rel,
                              f"description is {len(desc)} chars, target "
                              f"{DESC_MIN}-{DESC_MAX} (rule 28)")
            if len(re.findall(r'<meta\b[^>]*name\s*=\s*["\']description["\']',
                              head, re.I)) > 1:
                self.gate("desc.multiple", rel, "more than one description (rule 35)")

            # -- keywords (29)
            if re.search(r'<meta\b[^>]*name\s*=\s*["\']keywords["\']', src, re.I):
                self.gate("meta.keywords", rel,
                          "<meta name=keywords> — Google: 'no effect on indexing "
                          "and ranking at all' (rule 29)")

            # -- charset / viewport (34)
            if not re.search(r"<meta\b[^>]*charset", head, re.I):
                self.gate("charset.missing", rel, "no <meta charset> (rule 34)")
            if not page.viewport:
                self.gate("viewport.missing", rel, "no viewport meta (rule 34)")

            # -- Open Graph + Twitter (30, 31, 32)
            for k in ("og:title", "og:type", "og:url", "og:image", "og:description"):
                if not self._meta(page, k):
                    self.gate("og.missing", rel, f"no {k} (rule 30)")
            og_url = self._meta(page, "og:url")
            if og_url and og_url != url:
                self.gate("og.url_mismatch", rel,
                          f"og:url {og_url} != canonical {url} (rule 20)")
            og_img = self._meta(page, "og:image")
            if og_img:
                if not og_img.startswith("https://"):
                    self.gate("og.image_relative", rel,
                              f"og:image {og_img} is not absolute https (rule 31)")
                else:
                    asset = urllib.parse.urlsplit(og_img).path.lstrip("/")
                    if not (self.root / asset).is_file():
                        self.gate("og.image_missing", rel,
                                  f"og:image {og_img} has no file on disk (rule 31)")
            card = self._meta(page, "twitter:card")
            if card not in ("summary", "summary_large_image"):
                self.gate("twitter.card", rel,
                          f"twitter:card is {card!r} (rule 32)")

            # -- robots meta (33, 36, 37)
            robots = (page.robots or "").lower()
            if robots:
                tokens = {t.strip() for t in robots.split(",")}
                if tokens <= {"index", "follow"} and tokens:
                    self.gate("robots.noop", rel,
                              f"meta robots {robots!r} is the default — a no-op "
                              f"tag whose only effect is a chance to typo "
                              f"'noindex' (rule 33)")
            declared_noindex = "noindex" in robots
            if declared_noindex != urlmap.is_noindex(rel):
                self.gate("robots.noindex_drift", rel,
                          f"page {'declares' if declared_noindex else 'does not declare'}"
                          f" noindex but urlmap.NOINDEX says otherwise")
            if declared_noindex and urlmap.in_sitemap(rel):
                self.gate("robots.noindex_in_sitemap", rel,
                          "noindex page is advertised in sitemap.xml (rule 36)")

            # -- structured data (40-48)
            self._check_jsonld(rel, page, url, title, desc)

            # -- fonts (56, 57)
            self._check_fonts(rel, src, head)

            # -- images (58, 59)
            for img in page.images:
                if img.get("alt") is None:
                    self._img_issue("img.no_alt", rel,
                                    f"line {img['line']}: <img> without alt (rule 59)")
                if not img.get("width") or not img.get("height"):
                    self._img_issue("img.no_dims", rel,
                                    f"line {img['line']}: <img> without width/height "
                                    f"(rule 58)")

        for t, rels in titles.items():
            if len(rels) > 1:
                self.gate("title.duplicate", rels[0],
                          f"<title> {t!r} is shared with {', '.join(rels[1:])} (rule 24)")
        for d, rels in descs.items():
            if len(rels) > 1:
                self.gate("desc.duplicate", rels[0],
                          f"description shared with {', '.join(rels[1:])} (rule 27)")

    def check_markdown_twins(self) -> None:
        """Check the HTML alternate contract without treating twins as pages."""
        for rel, src in self.raw.items():
            if not re.fullmatch(r"(?:answers|compare)/(?!index\.html$)[a-z0-9-]+\.html", rel):
                continue
            expected_href = "/" + rel[:-len(".html")] + ".md"
            twin = self.root / expected_href.lstrip("/")
            if not twin.is_file():
                self.gate("markdown.missing", rel,
                          f"Markdown twin {expected_href} is missing")
            tags = re.findall(r"<link\b[^>]*>", _head_of(src), re.I)
            alternates = []
            for tag in tags:
                rel_match = re.search(r"\brel\s*=\s*[\"']([^\"']*)[\"']", tag, re.I)
                type_match = re.search(r"\btype\s*=\s*[\"']([^\"']*)[\"']", tag, re.I)
                href_match = re.search(r"\bhref\s*=\s*[\"']([^\"']*)[\"']", tag, re.I)
                rels = (rel_match.group(1).lower().split() if rel_match else [])
                if "alternate" in rels and type_match and type_match.group(1).lower() == "text/markdown":
                    alternates.append(href_match.group(1) if href_match else "")
            if alternates != [expected_href]:
                self.gate("markdown.alternate", rel,
                          f"expected one text/markdown alternate href {expected_href!r}; got {alternates!r}")
            if re.search(r"<a\b[^>]*\bhref\s*=\s*[\"'](?![A-Za-z][A-Za-z0-9+.-]*:|//)[^\"']+\.md(?:[?#][^\"']*)?[\"']",
                         src, re.I):
                self.gate("markdown.ordinary_link", rel,
                          "Markdown twins must be referenced with <link rel=alternate>, not <a>")

    def _img_issue(self, rule: str, rel: str, msg: str) -> None:
        (self.warn if IMG_RULES_ARE_WARNINGS else self.gate)(rule, rel, msg)

    def _check_fonts(self, rel: str, src: str, head: str) -> None:
        if "fonts.googleapis.com" not in src:
            return
        if not re.search(r'<link[^>]*rel=["\']preconnect["\'][^>]*'
                         r'fonts\.googleapis\.com', head, re.I):
            self.gate("fonts.preconnect", rel,
                      "loads fonts.googleapis.com without a preconnect (rule 56)")
        m = re.search(r'<link[^>]*fonts\.gstatic\.com[^>]*>', head, re.I)
        if not m or "crossorigin" not in m.group(0).lower():
            self.gate("fonts.preconnect_crossorigin", rel,
                      "fonts.gstatic.com preconnect missing or lacks the required "
                      "crossorigin attribute (rule 56)")
        for u in re.findall(r'https://fonts\.googleapis\.com/css2\?[^"\']+', src):
            if "display=swap" not in u:
                self.gate("fonts.display_swap", rel,
                          "Google Fonts URL lacks display=swap (rule 57)")

    def _check_jsonld(self, rel: str, page: audit_mod.Page, url: str,
                      title: str, desc: str) -> None:
        if not page.jsonld:
            self.gate("jsonld.missing", rel, "no application/ld+json block (rule 40)")
            return

        types: set[str] = set()
        crumbs: list[dict] = []
        articles: list[dict] = []
        for raw, line in page.jsonld:
            try:
                obj = json.loads(raw)
            except json.JSONDecodeError as e:
                self.gate("jsonld.invalid", rel,
                          f"line {line}: block does not parse as JSON: {e} (rule 40)")
                continue
            for node in obj if isinstance(obj, list) else [obj]:
                if not isinstance(node, dict):
                    self.gate("jsonld.not_object", rel,
                              f"line {line}: JSON-LD is not an object (rule 41)")
                    continue
                if node.get("@context") != "https://schema.org":
                    self.gate("jsonld.context", rel,
                              f"line {line}: @context is "
                              f"{node.get('@context')!r} (rule 41)")
                t = node.get("@type")
                if not t:
                    self.gate("jsonld.no_type", rel,
                              f"line {line}: no @type (rule 41)")
                    continue
                types.add(t)
                if t in ("FAQPage", "HowTo"):
                    self.gate("jsonld.deprecated_type", rel,
                              f"line {line}: {t} — FAQ rich results end 7 May 2026 "
                              f"and HowTo was removed outright (rule 46)")
                if "aggregateRating" in node or "review" in node:
                    self.gate("jsonld.fabricated_rating", rel,
                              f"line {line}: aggregateRating/review without a real "
                              f"review corpus is a manual-action risk")
                if t == "BreadcrumbList":
                    crumbs.append(node)
                if t in ("Article", "TechArticle", "NewsArticle", "BlogPosting"):
                    articles.append(node)

        # rule 42
        if rel != "index.html" and not crumbs:
            self.gate("jsonld.no_breadcrumb", rel,
                      "page below the root has no BreadcrumbList (rule 42)")

        # rules 43, 44, 45
        for bc in crumbs:
            items = bc.get("itemListElement") or []
            if not isinstance(items, list) or not items:
                self.gate("breadcrumb.empty", rel, "BreadcrumbList has no items (rule 43)")
                continue
            for i, it in enumerate(items):
                pos = it.get("position")
                if pos != i + 1:
                    self.gate("breadcrumb.position", rel,
                              f"item {i}: position {pos!r}, expected {i + 1} (rule 43)")
                if not it.get("name"):
                    self.gate("breadcrumb.name", rel, f"item {i} has no name (rule 44)")
                item_url = it.get("item")
                if i < len(items) - 1:
                    if not isinstance(item_url, str) or not item_url.startswith("https://"):
                        self.gate("breadcrumb.item", rel,
                                  f"item {i} needs an absolute item URL (rule 44)")
                        continue
                    if item_url.endswith(".html"):
                        self.gate("breadcrumb.item_html", rel,
                                  f"item {i}: {item_url} ends in .html (rule 45)")
                    target = urlmap.rel_for_url(item_url)
                    if target is None or not (self.root / target).is_file():
                        self.gate("breadcrumb.item_dead", rel,
                                  f"item {i}: {item_url} is not a real page (rule 45)")
                elif item_url is not None:
                    self.warn("breadcrumb.last_item", rel,
                              "the final ListItem should omit `item` so Google "
                              "uses the containing page URL (rule 44)")

        # rules 47, 48
        kind = pagedata.entry(rel)["kind"]
        if kind in pagedata.ARTICLE_KINDS:
            if not articles:
                self.gate("jsonld.no_article", rel,
                          f"{kind} page carries no Article/TechArticle (rule 47)")
            for a in articles:
                for field in ("headline", "description", "datePublished",
                              "dateModified", "mainEntityOfPage"):
                    if not a.get(field):
                        self.gate("article.field", rel,
                                  f"Article is missing {field} (rule 47)")
                meop = a.get("mainEntityOfPage")
                meop_url = meop.get("@id") if isinstance(meop, dict) else meop
                if meop_url != url:
                    self.gate("article.main_entity", rel,
                              f"mainEntityOfPage {meop_url!r} != canonical {url} "
                              f"(rule 48)")

    # ── internal link graph (rules 21, 49, 50, 51, 52, 53, 54) ──────────────
    def check_links(self) -> None:
        graph: dict[str, set[str]] = {rel: set() for rel in self.pages}
        anchor_text: dict[str, list[tuple[str, str]]] = collections.defaultdict(list)

        for rel, page in self.pages.items():
            src_dir = pathlib.PurePosixPath(rel).parent
            for href, line, _rel_attr in page.links:
                href = html.unescape(href.strip())
                if not urlmap.is_internal_href(href):
                    continue
                path = href.split("#", 1)[0].split("?", 1)[0]
                if not path:
                    continue
                if path.endswith(".html"):
                    self.gate("link.html_form", rel,
                              f"line {line}: href {href!r} costs a 308 — link the "
                              f"served URL instead (rule 21)")
                target = self._resolve(path, src_dir)
                if target is None:
                    if path in KNOWN_BROKEN:
                        self.warn("link.known_broken", rel,
                                  f"line {line}: {path} — {KNOWN_BROKEN[path]}")
                    else:
                        self.gate("link.broken", rel,
                                  f"line {line}: {href!r} resolves to nothing (rule 51)")
                    continue
                if target.endswith(".html") and target in self.pages:
                    graph[rel].add(target)

            for tag in re.finditer(r"<a\b[^>]*>(.*?)</a>", self.raw[rel], re.I | re.S):
                text = " ".join(re.sub(r"<[^>]+>", " ", tag.group(1)).split()).lower()
                if text in GENERIC_ANCHORS:
                    anchor_text[rel].append((text, tag.group(0)[:60]))

        # rules 49, 50 — BFS from the homepage
        depth = {"index.html": 0}
        queue = collections.deque(["index.html"])
        while queue:
            cur = queue.popleft()
            for nxt in sorted(graph.get(cur, ())):
                if nxt not in depth:
                    depth[nxt] = depth[cur] + 1
                    queue.append(nxt)
        for rel in sorted(self.pages):
            if not urlmap.in_sitemap(rel) and rel not in depth:
                continue  # deliberately unlisted pages need no crawl path
            if rel not in depth:
                self.gate("link.orphan", rel,
                          "unreachable from index.html by following <a href> (rule 49)")
            elif depth[rel] > MAX_DEPTH:
                self.gate("link.deep", rel,
                          f"{depth[rel]} clicks from the homepage (max {MAX_DEPTH}) "
                          f"(rule 50)")

        # rule 54 — every page is linked from a hub at or above its directory
        inbound_from_hub: dict[str, bool] = {rel: False for rel in self.pages}
        for rel in self.pages:
            hub = self._hub_for(rel)
            if hub is None:
                inbound_from_hub[rel] = True
                continue
            inbound_from_hub[rel] = rel in graph.get(hub, set()) or hub == rel
        for rel, ok in sorted(inbound_from_hub.items()):
            if ok or not urlmap.in_sitemap(rel):
                continue
            self.gate("link.hub_gap", rel,
                      f"section hub {self._hub_for(rel)} does not link to it (rule 54)")

        # rules 52, 53 — warnings
        inbound = collections.Counter()
        for rel, outs in graph.items():
            for t in outs:
                inbound[t] += 1
        for rel in sorted(self.pages):
            if pagedata.entry(rel)["kind"] in pagedata.ARTICLE_KINDS:
                if len(graph[rel]) < 3:
                    self.warn("link.few_outbound", rel,
                              f"{len(graph[rel])} outbound internal links (<3) (rule 52)")
                if inbound[rel] < 1:
                    self.warn("link.no_inbound", rel, "no inbound internal link (rule 52)")
        for rel, hits in sorted(anchor_text.items()):
            for text, snippet in hits:
                self.warn("link.generic_anchor", rel,
                          f"anchor text {text!r} in {snippet!r} (rule 53)")

    def _hub_for(self, rel: str) -> str | None:
        """The nearest hub page at or above `rel`'s directory."""
        parts = rel.split("/")[:-1]
        while parts:
            d = "/".join(parts)
            for cand in (f"{d}/index.html", f"{d}.html"):
                if cand in self.pages and cand != rel:
                    return cand
            parts.pop()
        return "index.html" if rel != "index.html" else None

    def _resolve(self, path: str, src_dir: pathlib.PurePosixPath) -> str | None:
        """Map an internal href to a repo-relative file, or None if it 404s."""
        if path.startswith("/"):
            base = path.lstrip("/")
        else:
            base = str((src_dir / path)).lstrip("./")
            base = str(pathlib.PurePosixPath(base))
        candidates = [base]
        if base.endswith("/") or base == "":
            candidates = [base + "index.html"]
        else:
            candidates += [base + ".html", base + "/index.html"]
        for c in candidates:
            if c and (self.root / c).is_file():
                return c
        return None

    # -- runner -------------------------------------------------------------
    def run(self) -> list[Issue]:
        self.check_sitemap()
        self.check_robots()
        self.check_headers()
        self.check_published_urls()
        self.check_pinned_refs()
        self.check_intraword_emphasis()
        self.check_not_found()
        self.check_pages()
        self.check_markdown_twins()
        self.check_links()
        return self.issues


def _head_of(src: str) -> str:
    m = re.search(r"<head\b[^>]*>(.*?)</head\s*>", src, re.I | re.S)
    return m.group(1) if m else ""


def _today() -> str:
    import datetime
    return datetime.date.today().isoformat()


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", default=None, help="landing directory")
    ap.add_argument("--warnings-as-errors", action="store_true")
    ap.add_argument("--quiet", action="store_true", help="only print the summary")
    args = ap.parse_args(argv)

    root = pathlib.Path(args.root) if args.root else urlmap.landing_dir()
    lint = Lint(root, urlmap.repo_root())
    issues = lint.run()

    gates = [i for i in issues if i.level == "gate"]
    warns = [i for i in issues if i.level == "warn"]

    if not args.quiet:
        for i in sorted(gates, key=lambda x: (x.rule, x.path)):
            print(i)
        for i in sorted(warns, key=lambda x: (x.rule, x.path)):
            print(i)

    by_rule = collections.Counter(i.rule for i in gates)
    print(f"\nseo_lint: {len(lint.pages)} pages · "
          f"{len(gates)} gate violation(s) · {len(warns)} warning(s)")
    for rule, n in by_rule.most_common():
        print(f"  gate {rule}: {n}")

    if gates:
        return 1
    if warns and args.warnings_as_errors:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
