#!/usr/bin/env python3
"""audit.py — dependency-free technical SEO audit for the static xerj.org site.

The site is ~80 hand-written HTML files under landing/, deployed verbatim by
Cloudflare Pages (`pages_build_output_dir = "landing"`). There is no static
site generator, so there is no build step that could enforce head hygiene,
canonicals, structured data or sitemap coverage. This script is that step.

It walks the site root, parses every .html with the stdlib HTML parser, and
emits BOTH:

  * a human-readable report on stdout (or --report FILE), severity-ranked
  * a machine-readable JSON summary (--json FILE)

It is re-runnable in CI and exits non-zero once findings at or above a chosen
severity exceed a threshold, so it can gate a pull request later:

    scripts/seo/audit.py --root landing --fail-on critical --max-findings 0

Checks implemented (each finding carries file path + line number where the
markup is the source of truth):

   1. head hygiene   — title/description presence, length, cross-page dupes;
                       <html lang>; viewport meta
   2. canonical      — presence, self-reference, conflicts
   3. social         — og:title/description/url/image/type, twitter:card,
                       and the "partial set" case (worse than none)
   4. structured     — JSON-LD presence, JSON validity, @context/@type
   5. sitemap        — disk-vs-sitemap both ways, URL form mismatches,
                       <lastmod> vs the file's real last-change date
   6. headings       — missing/multiple H1, skipped levels, empty headings
   7. links          — broken internal links, orphan pages, click depth from
                       the homepage, .html links that shadow a directory index
   8. images         — missing alt, missing width/height (CLS), oversized files
   9. thinness       — visible word count, boilerplate-adjusted
  10. duplication    — cross-page shingle overlap; size of repeated inline
                       JSON blobs (e.g. the docs search index)
  11. robots         — meta robots noindex/nofollow; robots.txt vs sitemap
  12. weight         — page weight incl. first-order assets, render-blocking
                       resources, third-party requests

Design notes
------------
* Stdlib only (html.parser, urllib.parse, xml.etree, json, subprocess). No
  network access: every check is answerable from the working tree.
* `git log -1 --format=%cs -- <file>` supplies the real last-change date for
  the <lastmod> comparison. Some checkouts in this workspace have broken git
  metadata; when git fails the script falls back to filesystem mtime and says
  so, in the report and in the JSON (`meta.date_source`).
* Nothing is written to the site. This tool measures; it never fixes.
"""

from __future__ import annotations

import argparse
import collections
import dataclasses
import html
import html.parser
import json
import os
import pathlib
import re
import subprocess
import sys
import urllib.parse
import xml.etree.ElementTree as ET
import zlib

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import urlmap as _urlmap  # noqa: E402  (the one definition of the deployed URL shape)

# ── tunables ────────────────────────────────────────────────────────────────

SITE_ORIGIN = "https://xerj.org"

TITLE_MAX = 60
DESC_MAX = 160
DESC_MIN = 50
THIN_WORDS = 300
MAX_DEPTH = 3
IMAGE_MAX_BYTES = 200 * 1024
PAGE_WEIGHT_WARN = 1024 * 1024
SHINGLE_N = 5
DUPLICATE_JACCARD = 0.60
BOILERPLATE_PAGE_FRACTION = 0.50

SEVERITIES = ("critical", "high", "medium", "low")

# Text-bearing elements are ignored inside these — they are not rendered text
# and Google does not treat them as page content.
NON_TEXT_TAGS = {"script", "style", "noscript", "template", "svg", "head", "title"}

VOID_TAGS = {
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link",
    "meta", "param", "source", "track", "wbr",
}

# Checks, with their severity and the one-line "why it matters" used by the
# report generator. Keeping this in one table means the report, the JSON and
# the exit code all agree on severity by construction.
CHECKS: dict[str, tuple[str, str]] = {
    # id                          severity    what it is
    "title.missing":            ("critical", "page has no <title>"),
    "title.duplicate":          ("high",     "<title> is byte-identical to another page's"),
    "title.long":               ("medium",   f"<title> longer than {TITLE_MAX} chars (truncated in SERPs)"),
    "title.empty":              ("critical", "<title> present but empty"),
    "desc.missing":             ("high",     "no <meta name=\"description\">"),
    "desc.duplicate":           ("high",     "meta description duplicated across pages"),
    "desc.long":                ("medium", f"meta description longer than {DESC_MAX} chars"),
    "desc.short":               ("low",      f"meta description shorter than {DESC_MIN} chars"),
    "html.lang.missing":        ("medium",   "<html> has no lang attribute"),
    "viewport.missing":         ("medium",   "no responsive viewport meta"),
    "canonical.missing":        ("high",     "no self-referencing <link rel=canonical>"),
    "canonical.mismatch":       ("high",     "canonical does not match the file's deployed URL"),
    "canonical.conflict":       ("critical", "more than one <link rel=canonical>"),
    "og.missing":               ("medium",   "no Open Graph tags at all"),
    "og.partial":               ("medium",   "incomplete Open Graph set (unpredictable link previews)"),
    "twitter.card.missing":     ("medium",   "no twitter:card"),
    "jsonld.missing":           ("high",     "no application/ld+json structured data"),
    "jsonld.invalid":           ("critical", "application/ld+json block does not parse as JSON"),
    "jsonld.no_type":           ("high",     "JSON-LD block missing @context or @type"),
    "sitemap.missing_page":     ("high",     "indexable page absent from sitemap.xml"),
    "sitemap.dead_url":         ("critical", "sitemap.xml URL resolves to no file on disk"),
    "sitemap.markdown":          ("critical", "sitemap.xml advertises an alternate Markdown twin"),
    "sitemap.form_mismatch":    ("medium",   "sitemap URL form differs from the canonical deployed form"),
    "sitemap.stale_lastmod":    ("low",      "<lastmod> disagrees with the file's real last-change date"),
    "h1.missing":               ("high",     "page has no <h1>"),
    "h1.multiple":              ("medium",   "more than one <h1>"),
    "heading.skip":             ("medium",   "heading level skipped (e.g. h2 → h4)"),
    "heading.empty":            ("low",      "heading element with no text"),
    "link.broken":              ("critical", "internal link resolves to no file"),
    "link.orphan":              ("high",     "page is reachable from no other page"),
    "link.deep":                ("medium",   f"page is more than {MAX_DEPTH} clicks from the homepage"),
    "link.html_shadows_dir":    ("medium",   "links to /x.html where /x/ also exists (duplicate paths)"),
    "img.no_alt":               ("medium",   "<img> without alt attribute"),
    "img.no_dims":              ("low",      "<img> without width/height (layout shift)"),
    "img.oversized":            ("medium",   f"image file larger than {IMAGE_MAX_BYTES // 1024} KB"),
    "content.thin":             ("medium",   f"fewer than {THIN_WORDS} words of visible text"),
    "content.duplicate":        ("high",     "visible text overlaps heavily with another page"),
    "robots.noindex":           ("critical", "meta robots noindex on a page the sitemap advertises"),
    "robots.noindex.ok":        ("low",      "meta robots noindex (page is not in the sitemap — looks deliberate)"),
    "robots.nofollow":          ("high",     "meta robots nofollow"),
    "robots.conflict":          ("high",     "robots.txt disallows a URL the sitemap advertises"),
    "weight.page":              ("low",      "page weight above 1 MB including first-order assets"),
    "weight.render_blocking":   ("medium",   "render-blocking third-party stylesheet/script"),
    "redirect.collision":       ("high",     "_redirects source path also exists as a real file/directory"),
    "content.boilerplate":      ("medium",   "over half of the page's visible text is site-wide boilerplate"),
    "markdown.missing":          ("high",     "article has no Markdown twin"),
    "markdown.alternate":        ("high",     "article has no exact text/markdown alternate link"),
    "markdown.ordinary_link":    ("medium",   "Markdown twin is linked as ordinary page content"),
}


def severity_of(check_id: str) -> str:
    return CHECKS.get(check_id, ("low", ""))[0]


# ── data model ──────────────────────────────────────────────────────────────


@dataclasses.dataclass
class Finding:
    check: str
    severity: str
    path: str       # site-relative posix path, e.g. "docs/cli.html"
    line: int       # 0 when the finding is about the file as a whole
    message: str
    detail: dict | None = None

    def as_dict(self) -> dict:
        d = {
            "check": self.check,
            "severity": self.severity,
            "path": self.path,
            "line": self.line,
            "message": self.message,
        }
        if self.detail:
            d["detail"] = self.detail
        return d


@dataclasses.dataclass
class Page:
    path: str
    abs_path: pathlib.Path
    size: int = 0
    lang: str | None = None
    title: str | None = None
    title_line: int = 0
    titles_seen: int = 0
    description: str | None = None
    description_line: int = 0
    viewport: bool = False
    canonicals: list = dataclasses.field(default_factory=list)      # (href, line)
    metas: dict = dataclasses.field(default_factory=dict)           # name/property -> content
    meta_lines: dict = dataclasses.field(default_factory=dict)
    robots: str | None = None
    headings: list = dataclasses.field(default_factory=list)        # (level, text, line)
    links: list = dataclasses.field(default_factory=list)           # (href, line, rel)
    images: list = dataclasses.field(default_factory=list)          # dict
    jsonld: list = dataclasses.field(default_factory=list)          # (raw, line)
    stylesheets: list = dataclasses.field(default_factory=list)     # (href, line, in_head, blocking)
    scripts: list = dataclasses.field(default_factory=list)         # (src, line, in_head, blocking)
    inline_json: list = dataclasses.field(default_factory=list)     # (element_id, bytes, line)
    text_chunks: list = dataclasses.field(default_factory=list)
    words: list = dataclasses.field(default_factory=list)
    # derived
    word_count: int = 0
    unique_word_count: int = 0
    asset_bytes: int = 0
    depth: int | None = None
    inbound: int = 0

    @property
    def url_path(self) -> str:
        """Canonical deployed path for this file (directory form for indexes).

        CORRECTION 2026-08-20: this used to return `/product.html` for
        `product.html`, i.e. it treated the `.html` spelling as canonical.
        That contradicts both this audit's own headline finding and the
        docstring of `acceptable_urls` below: Cloudflare Pages 308-redirects
        `/product.html` -> `/product`, so the `.html` form is never the URL
        that returns 200.  With the old behaviour every correctly-generated
        sitemap raised a false `sitemap.form_mismatch`.

        The mapping now comes from `scripts/seo/urlmap.py`, which is the one
        place the canonical URL shape is defined for the whole SEO toolchain.
        """
        return _urlmap.deployed_path(self.path)

    @property
    def canonical_url(self) -> str:
        return SITE_ORIGIN + self.url_path

    def acceptable_urls(self) -> set[str]:
        """Every URL form Cloudflare Pages will serve this file at.

        Pages serves `/foo.html`, redirects it to `/foo`, and serves
        `/dir/index.html` at `/dir/`. All of these name the same document, so
        a canonical pointing at any of them is *self*-referencing; only the
        inconsistency between them is reported (sitemap.form_mismatch).
        """
        out = {self.canonical_url}
        p = self.path
        if p.endswith("/index.html") or p == "index.html":
            out.add(SITE_ORIGIN + "/" + p)                       # explicit index.html
            if self.url_path != "/":
                out.add(SITE_ORIGIN + self.url_path.rstrip("/"))  # no trailing slash
        else:
            out.add(SITE_ORIGIN + "/" + p[: -len(".html")])       # extensionless
        # trailing-slash / origin-slash variants
        out.add(self.canonical_url.rstrip("/") or SITE_ORIGIN + "/")
        return out


# ── parser ──────────────────────────────────────────────────────────────────


class PageParser(html.parser.HTMLParser):
    """Collects everything the checks need in a single pass.

    Line numbers come from HTMLParser.getpos(), which is 1-based and points at
    the opening tag — exactly what a human needs to find the markup.
    """

    def __init__(self, page: Page):
        super().__init__(convert_charrefs=True)
        self.page = page
        self.stack: list[str] = []
        self.suppress = 0          # depth inside non-text elements
        self.in_head = False
        self.cur_heading: list | None = None
        self.cur_script: dict | None = None
        self.script_buf: list[str] = []
        self.in_title = False

    # -- helpers
    def _line(self) -> int:
        return self.getpos()[0]

    @staticmethod
    def _attrs(attrs) -> dict:
        return {k.lower(): (v if v is not None else "") for k, v in attrs}

    # -- tag handlers
    def handle_starttag(self, tag, attrs):
        tag = tag.lower()
        a = self._attrs(attrs)
        line = self._line()
        p = self.page

        if tag not in VOID_TAGS:
            self.stack.append(tag)
        if tag in NON_TEXT_TAGS:
            self.suppress += 1

        if tag == "html":
            p.lang = a.get("lang")
        elif tag == "head":
            self.in_head = True
        elif tag == "title":
            p.titles_seen += 1
            self.in_title = p.titles_seen == 1
            if p.title is None:
                p.title_line = line
                p.title = ""
        elif tag == "meta":
            key = (a.get("name") or a.get("property") or "").lower()
            content = a.get("content", "")
            if key:
                p.metas.setdefault(key, content)
                p.meta_lines.setdefault(key, line)
            if key == "description" and p.description is None:
                p.description, p.description_line = content, line
            elif key == "viewport":
                p.viewport = True
            elif key == "robots":
                p.robots = content.lower()
            elif a.get("charset") is not None:
                pass
        elif tag == "link":
            rels = (a.get("rel") or "").lower().split()
            href = a.get("href", "")
            if "canonical" in rels:
                p.canonicals.append((href, line))
            if "stylesheet" in rels:
                blocking = "print" not in (a.get("media") or "")
                p.stylesheets.append((href, line, self.in_head, blocking))
            if "preload" in rels or "preconnect" in rels or "dns-prefetch" in rels:
                pass
            if href and "canonical" not in rels and "stylesheet" not in rels:
                pass
        elif tag == "script":
            stype = (a.get("type") or "").lower()
            src = a.get("src", "")
            blocking = src != "" and "async" not in a and "defer" not in a and (
                (a.get("type") or "").lower() != "module"
            )
            if src:
                p.scripts.append((src, line, self.in_head, blocking))
            self.cur_script = {"type": stype, "id": a.get("id", ""), "line": line, "src": src}
            self.script_buf = []
        elif tag in ("h1", "h2", "h3", "h4", "h5", "h6"):
            self.cur_heading = [int(tag[1]), [], line]
        elif tag == "a":
            href = a.get("href")
            if href is not None:
                p.links.append((href.strip(), line, (a.get("rel") or "").lower()))
        elif tag == "img":
            p.images.append({
                "src": a.get("src", ""),
                "line": line,
                "alt": a.get("alt"),
                "width": a.get("width"),
                "height": a.get("height"),
                "loading": a.get("loading"),
            })

    def handle_startendtag(self, tag, attrs):
        self.handle_starttag(tag, attrs)
        if tag.lower() in NON_TEXT_TAGS:
            self.suppress = max(0, self.suppress - 1)

    def handle_endtag(self, tag):
        tag = tag.lower()
        p = self.page
        if tag in NON_TEXT_TAGS:
            self.suppress = max(0, self.suppress - 1)
        if tag == "title":
            self.in_title = False
        elif tag == "head":
            self.in_head = False
        elif tag == "script" and self.cur_script is not None:
            raw = "".join(self.script_buf)
            st = self.cur_script["type"]
            if st == "application/ld+json":
                p.jsonld.append((raw, self.cur_script["line"]))
            elif st in ("application/json", "importmap") or st.endswith("+json"):
                p.inline_json.append((self.cur_script["id"], len(raw.encode("utf-8")),
                                      self.cur_script["line"]))
            self.cur_script = None
            self.script_buf = []
        elif tag in ("h1", "h2", "h3", "h4", "h5", "h6") and self.cur_heading:
            level, parts, line = self.cur_heading
            p.headings.append((level, " ".join("".join(parts).split()), line))
            self.cur_heading = None

        # unwind the stack to the matching open tag (tolerant of bad nesting)
        if tag in self.stack:
            while self.stack:
                t = self.stack.pop()
                if t == tag:
                    break

    def handle_data(self, data):
        if self.cur_script is not None:
            self.script_buf.append(data)
            return
        if self.in_title:
            self.page.title = (self.page.title or "") + data
            return
        if self.cur_heading is not None:
            self.cur_heading[1].append(data)
        if self.suppress:
            return
        chunk = " ".join(data.split())
        if chunk:
            self.page.text_chunks.append(chunk)


WORD_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9'’\-]*")


# ── site model ──────────────────────────────────────────────────────────────


class Audit:
    def __init__(self, root: pathlib.Path, origin: str = SITE_ORIGIN):
        self.root = root.resolve()
        self.origin = origin.rstrip("/")
        self.pages: dict[str, Page] = {}
        self.raw: dict[str, str] = {}
        self.findings: list[Finding] = []
        self.notes: list[str] = []
        self.date_source = "git"
        self.redirects: dict[str, str] = {}
        self.sitemap_entries: list[tuple[str, str | None]] = []
        self.robots_rules: list[tuple[str, str]] = []
        self.stats: dict = {}
        self._size_cache: dict[pathlib.Path, int] = {}

    # -- infrastructure
    def rel(self, p: pathlib.Path) -> str:
        return p.relative_to(self.root).as_posix()

    def file_size(self, p: pathlib.Path) -> int:
        if p not in self._size_cache:
            try:
                self._size_cache[p] = p.stat().st_size
            except OSError:
                self._size_cache[p] = 0
        return self._size_cache[p]

    def add(self, check: str, path: str, line: int, message: str, detail=None):
        self.findings.append(Finding(check, severity_of(check), path, line, message, detail))

    # -- loading
    def load(self):
        for p in sorted(self.root.rglob("*.html")):
            if any(part.startswith(".") for part in p.relative_to(self.root).parts):
                continue
            self.raw[self.rel(p)] = p.read_text(encoding="utf-8", errors="replace")
            self.pages[self.rel(p)] = self._parse(p)
        self._load_redirects()
        self._load_sitemap()
        self._load_robots()

    def _parse(self, abs_path: pathlib.Path) -> Page:
        page = Page(path=self.rel(abs_path), abs_path=abs_path)
        raw = abs_path.read_text(encoding="utf-8", errors="replace")
        page.size = len(raw.encode("utf-8"))
        parser = PageParser(page)
        try:
            parser.feed(raw)
            parser.close()
        except Exception as exc:  # malformed markup should not kill the run
            self.notes.append(f"{page.path}: parser error ({exc})")
        page.words = WORD_RE.findall(" ".join(page.text_chunks).lower())
        page.word_count = len(page.words)
        if page.title is not None:
            page.title = " ".join(page.title.split())
        return page

    def _load_redirects(self):
        f = self.root / "_redirects"
        if not f.exists():
            return
        for raw in f.read_text(encoding="utf-8", errors="replace").splitlines():
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            parts = line.split()
            if len(parts) >= 2:
                self.redirects[parts[0]] = parts[1]

    def _load_sitemap(self):
        f = self.root / "sitemap.xml"
        if not f.exists():
            self.notes.append("no sitemap.xml found at site root")
            return
        try:
            tree = ET.fromstring(f.read_text(encoding="utf-8", errors="replace"))
        except ET.ParseError as exc:
            self.notes.append(f"sitemap.xml does not parse: {exc}")
            return
        ns = {"sm": "http://www.sitemaps.org/schemas/sitemap/0.9"}
        for url in tree.findall("sm:url", ns) or tree.findall("url"):
            loc = url.find("sm:loc", ns)
            if loc is None:
                loc = url.find("loc")
            lastmod = url.find("sm:lastmod", ns)
            if lastmod is None:
                lastmod = url.find("lastmod")
            if loc is not None and loc.text:
                self.sitemap_entries.append(
                    (loc.text.strip(), lastmod.text.strip() if lastmod is not None and lastmod.text else None)
                )

    def _load_robots(self):
        f = self.root / "robots.txt"
        if not f.exists():
            self.notes.append("no robots.txt found at site root")
            return
        agent = "*"
        for raw in f.read_text(encoding="utf-8", errors="replace").splitlines():
            line = raw.split("#", 1)[0].strip()
            if not line or ":" not in line:
                continue
            key, _, val = line.partition(":")
            key, val = key.strip().lower(), val.strip()
            if key == "user-agent":
                agent = val
            elif key in ("disallow", "allow") and val:
                self.robots_rules.append((key, val))

    # -- URL <-> file resolution
    def resolve_url_path(self, url_path: str) -> pathlib.Path | None:
        """Map a site-absolute URL path to the file Cloudflare Pages serves."""
        url_path = urllib.parse.unquote(url_path)
        if url_path in self.redirects:
            target = self.redirects[url_path]
            if target.startswith("http"):
                return None
            url_path = target
        clean = url_path.split("#", 1)[0].split("?", 1)[0]
        if not clean.startswith("/"):
            clean = "/" + clean
        rel = clean.lstrip("/")
        base = self.root
        if clean.endswith("/"):
            cand = base / rel / "index.html"
            return cand if cand.is_file() else None
        cand = base / rel
        if cand.is_file():
            return cand
        for extra in (rel + ".html", rel + "/index.html"):
            c = base / extra
            if c.is_file():
                return c
        return None

    # -- git / mtime dates
    def last_change_date(self, page_path: str) -> str | None:
        if self.date_source == "git":
            try:
                out = subprocess.run(
                    ["git", "log", "-1", "--format=%cs", "--", page_path],
                    cwd=self.root, capture_output=True, text=True, timeout=20,
                )
                if out.returncode == 0:
                    val = out.stdout.strip()
                    if val:
                        return val
                    return None  # file exists but is untracked/uncommitted
                self.date_source = "mtime"
                self.notes.append(
                    "git is unusable in this checkout ("
                    + (out.stderr.strip().splitlines() or ["no stderr"])[0]
                    + "); <lastmod> is compared against filesystem mtime instead"
                )
            except (OSError, subprocess.SubprocessError) as exc:
                self.date_source = "mtime"
                self.notes.append(
                    f"git is unusable in this checkout ({exc}); "
                    "<lastmod> is compared against filesystem mtime instead"
                )
        import datetime
        p = self.root / page_path
        if not p.exists():
            return None
        return datetime.date.fromtimestamp(p.stat().st_mtime).isoformat()

    # ── checks ──────────────────────────────────────────────────────────
    def run(self):
        self.check_head()
        self.check_canonical()
        self.check_social()
        self.check_structured_data()
        self.check_headings()
        self.check_links()
        self.check_redirect_collisions()
        self.check_images()
        self.check_content()
        self.check_sitemap()
        self.check_markdown_twins()
        self.check_robots()
        self.check_weight()

    # 1. head hygiene
    def check_head(self):
        by_title: dict[str, list[str]] = collections.defaultdict(list)
        by_desc: dict[str, list[str]] = collections.defaultdict(list)
        for path, pg in self.pages.items():
            if pg.title is None:
                self.add("title.missing", path, 0, "no <title> element")
            elif not pg.title.strip():
                self.add("title.empty", path, pg.title_line, "<title> is empty")
            else:
                by_title[pg.title].append(path)
                if len(pg.title) > TITLE_MAX:
                    self.add("title.long", path, pg.title_line,
                             f"<title> is {len(pg.title)} chars (>{TITLE_MAX})",
                             {"chars": len(pg.title), "title": pg.title})
            if pg.description is None:
                self.add("desc.missing", path, 0, "no <meta name=\"description\">")
            else:
                d = pg.description.strip()
                by_desc[d].append(path)
                if len(d) > DESC_MAX:
                    self.add("desc.long", path, pg.description_line,
                             f"meta description is {len(d)} chars (>{DESC_MAX})",
                             {"chars": len(d)})
                elif len(d) < DESC_MIN:
                    self.add("desc.short", path, pg.description_line,
                             f"meta description is only {len(d)} chars (<{DESC_MIN})",
                             {"chars": len(d), "description": d})
            if not pg.lang:
                self.add("html.lang.missing", path, 0, "<html> has no lang attribute")
            if not pg.viewport:
                self.add("viewport.missing", path, 0, "no <meta name=\"viewport\">")

        for title, paths in sorted(by_title.items()):
            if len(paths) > 1:
                for path in paths:
                    self.add("title.duplicate", path, self.pages[path].title_line,
                             f"<title> shared with {len(paths) - 1} other page(s)",
                             {"title": title, "shared_with": [p for p in paths if p != path]})
        for desc, paths in sorted(by_desc.items()):
            if len(paths) > 1:
                for path in paths:
                    self.add("desc.duplicate", path, self.pages[path].description_line,
                             f"meta description shared with {len(paths) - 1} other page(s)",
                             {"shared_with": [p for p in paths if p != path]})

    # 2. canonical
    def check_canonical(self):
        for path, pg in self.pages.items():
            if not pg.canonicals:
                self.add("canonical.missing", path, 0,
                         f"no canonical; expected <link rel=\"canonical\" href=\"{pg.canonical_url}\">",
                         {"expected": pg.canonical_url})
                continue
            if len(pg.canonicals) > 1:
                self.add("canonical.conflict", path, pg.canonicals[0][1],
                         f"{len(pg.canonicals)} canonical links",
                         {"hrefs": [h for h, _ in pg.canonicals]})
            href, line = pg.canonicals[0]
            abs_href = urllib.parse.urljoin(self.origin + pg.url_path, href)
            if abs_href not in pg.acceptable_urls():
                self.add("canonical.mismatch", path, line,
                         f"canonical {abs_href!r} != deployed URL {pg.canonical_url!r}",
                         {"found": abs_href, "expected": pg.canonical_url})

    # 3. social
    def check_social(self):
        og_required = ["og:title", "og:description", "og:url", "og:image", "og:type"]
        for path, pg in self.pages.items():
            present = [k for k in og_required if pg.metas.get(k)]
            missing = [k for k in og_required if not pg.metas.get(k)]
            if not present:
                self.add("og.missing", path, 0, "no Open Graph tags",
                         {"missing": og_required})
            elif missing:
                self.add("og.partial", path, min(pg.meta_lines.get(k, 0) for k in present),
                         f"has {len(present)}/{len(og_required)} og tags; missing {', '.join(missing)}",
                         {"present": present, "missing": missing})
            if not pg.metas.get("twitter:card"):
                self.add("twitter.card.missing", path, 0, "no twitter:card meta")

    # 4. structured data
    def check_structured_data(self):
        for path, pg in self.pages.items():
            if not pg.jsonld:
                self.add("jsonld.missing", path, 0, "no application/ld+json block")
                continue
            for raw, line in pg.jsonld:
                try:
                    data = json.loads(raw)
                except json.JSONDecodeError as exc:
                    self.add("jsonld.invalid", path, line, f"JSON-LD does not parse: {exc}")
                    continue
                blocks = data if isinstance(data, list) else [data]
                for block in blocks:
                    if not isinstance(block, dict) or "@type" not in block or "@context" not in block:
                        self.add("jsonld.no_type", path, line,
                                 "JSON-LD block missing @context and/or @type")

    # 6. headings
    def check_headings(self):
        for path, pg in self.pages.items():
            h1s = [h for h in pg.headings if h[0] == 1]
            if not h1s:
                self.add("h1.missing", path, 0, "no <h1> on the page")
            elif len(h1s) > 1:
                self.add("h1.multiple", path, h1s[1][2],
                         f"{len(h1s)} <h1> elements",
                         {"texts": [h[1][:80] for h in h1s]})
            prev = None
            for level, text, line in pg.headings:
                if not text:
                    self.add("heading.empty", path, line, f"empty <h{level}>")
                if prev is not None and level > prev + 1:
                    self.add("heading.skip", path, line,
                             f"h{prev} → h{level} skips a level",
                             {"from": prev, "to": level, "text": text[:80]})
                prev = level

    # 7. links
    def check_links(self):
        graph: dict[str, set[str]] = {p: set() for p in self.pages}
        dir_index_exists = {
            p[: -len("index.html")].rstrip("/") for p in self.pages if p.endswith("index.html")
        }
        external_hosts: collections.Counter = collections.Counter()

        for path, pg in self.pages.items():
            seen_broken = set()
            seen_shadow: set = set()
            for href, line, rel in pg.links:
                if not href or href.startswith(("#", "mailto:", "tel:", "javascript:", "data:")):
                    continue
                parts = urllib.parse.urlsplit(href)
                if parts.scheme in ("http", "https"):
                    if parts.netloc not in ("xerj.org", "www.xerj.org"):
                        external_hosts[parts.netloc] += 1
                        continue
                    target_path = parts.path or "/"
                elif parts.scheme:
                    continue
                else:
                    base = "/" + path
                    target_path = urllib.parse.urljoin(base, parts.path) if parts.path else "/" + path
                if not parts.path and parts.fragment:
                    continue
                resolved = self.resolve_url_path(target_path)
                if resolved is None:
                    key = (target_path, line)
                    if key not in seen_broken:
                        seen_broken.add(key)
                        self.add("link.broken", path, line,
                                 f"internal link {href!r} resolves to nothing",
                                 {"href": href, "resolved_path": target_path})
                    continue
                rp = self.rel(resolved)
                if rp in self.pages:
                    graph[path].add(rp)
                # /x.html link where /x/ also exists on disk
                stem = target_path[:-len(".html")].lstrip("/") if target_path.endswith(".html") else None
                if (stem and stem in dir_index_exists
                        and not target_path.endswith("/index.html")
                        and (path, stem) not in seen_shadow):
                    seen_shadow.add((path, stem))
                    self.add("link.html_shadows_dir", path, line,
                             f"link to /{stem}.html but /{stem}/ also exists "
                             f"(Pages strips .html, so both collapse to /{stem})",
                             {"href": href, "shadowed": f"/{stem}/"})

        # inbound counts, depth, orphans
        for src, dsts in graph.items():
            for d in dsts:
                if d != src:
                    self.pages[d].inbound += 1

        start = "index.html"
        if start in self.pages:
            depth = {start: 0}
            queue = collections.deque([start])
            while queue:
                cur = queue.popleft()
                for nxt in sorted(graph.get(cur, ())):
                    if nxt not in depth:
                        depth[nxt] = depth[cur] + 1
                        queue.append(nxt)
            for path, pg in self.pages.items():
                pg.depth = depth.get(path)
                if path == start:
                    continue
                if pg.depth is None:
                    self.add("link.orphan", path, 0,
                             "unreachable from index.html by following internal links",
                             {"inbound_links": pg.inbound})
                elif pg.depth > MAX_DEPTH:
                    self.add("link.deep", path, 0,
                             f"{pg.depth} clicks from the homepage (>{MAX_DEPTH})",
                             {"depth": pg.depth})
        self.stats["external_hosts"] = dict(external_hosts.most_common())
        self.stats["link_graph_edges"] = sum(len(v) for v in graph.values())

    # 8. images
    def check_images(self):
        oversized_reported = set()
        for path, pg in self.pages.items():
            for img in pg.images:
                if img["alt"] is None:
                    self.add("img.no_alt", path, img["line"],
                             f"<img src={img['src']!r}> has no alt attribute", {"src": img["src"]})
                if not img["width"] or not img["height"]:
                    self.add("img.no_dims", path, img["line"],
                             f"<img src={img['src']!r}> has no width/height", {"src": img["src"]})
                src = img["src"]
                if src and not src.startswith(("http", "data:")):
                    target = self.resolve_url_path(src if src.startswith("/") else
                                                   urllib.parse.urljoin("/" + path, src))
                    if target is not None:
                        size = self.file_size(target)
                        if size > IMAGE_MAX_BYTES and (path, src) not in oversized_reported:
                            oversized_reported.add((path, src))
                            self.add("img.oversized", path, img["line"],
                                     f"{src} is {size // 1024} KB (>{IMAGE_MAX_BYTES // 1024} KB)",
                                     {"src": src, "bytes": size})

    # 9 + 10. content thinness and duplication
    def check_content(self):
        chunk_pages: dict[str, set[str]] = collections.defaultdict(set)
        for path, pg in self.pages.items():
            for c in set(pg.text_chunks):
                chunk_pages[c].add(path)
        n = max(1, len(self.pages))
        boiler = {c for c, ps in chunk_pages.items()
                  if len(ps) >= BOILERPLATE_PAGE_FRACTION * n}
        boiler_words = 0
        for path, pg in self.pages.items():
            own = [c for c in pg.text_chunks if c not in boiler]
            pg.unique_word_count = len(WORD_RE.findall(" ".join(own).lower()))
            if pg.word_count and pg.unique_word_count / pg.word_count < 0.5:
                self.add("content.boilerplate", path, 0,
                         f"{pg.word_count - pg.unique_word_count}/{pg.word_count} words "
                         f"({100 - int(100 * pg.unique_word_count / pg.word_count)}%) are "
                         f"site-wide boilerplate",
                         {"words": pg.word_count, "unique_words": pg.unique_word_count,
                          "boilerplate_ratio": round(1 - pg.unique_word_count / pg.word_count, 3)})
            if pg.word_count < THIN_WORDS:
                self.add("content.thin", path, 0,
                         f"{pg.word_count} words of visible text "
                         f"({pg.unique_word_count} excluding site boilerplate)",
                         {"words": pg.word_count, "unique_words": pg.unique_word_count})
        self.stats["boilerplate_chunks"] = len(boiler)
        self.stats["boilerplate_words_per_page"] = boiler_words

        # shingles
        shingles: dict[str, set[int]] = {}
        for path, pg in self.pages.items():
            w = pg.words
            # zlib.crc32 rather than hash(): PYTHONHASHSEED randomises str hashing
            # per process, and a CI gate must give the same numbers every run.
            shingles[path] = {
                zlib.crc32(" ".join(w[i:i + SHINGLE_N]).encode("utf-8"))
                for i in range(max(0, len(w) - SHINGLE_N + 1))
            }
        paths = sorted(self.pages)
        pairs = []
        for i, a in enumerate(paths):
            sa = shingles[a]
            if len(sa) < 20:
                continue
            for b in paths[i + 1:]:
                sb = shingles[b]
                if len(sb) < 20:
                    continue
                inter = len(sa & sb)
                if not inter:
                    continue
                jac = inter / len(sa | sb)
                if jac >= 0.25:            # keep for the stats table
                    pairs.append((round(jac, 3), a, b))
        for jac, a, b in sorted(pairs, reverse=True):
            if jac < DUPLICATE_JACCARD:
                continue
            self.add("content.duplicate", a, 0,
                     f"{int(jac * 100)}% shingle overlap with {b}",
                     {"other": b, "jaccard": jac})
        self.stats["max_shingle_overlap"] = max((p[0] for p in pairs), default=0.0)
        self.stats["near_duplicate_pairs"] = [
            {"a": a, "b": b, "jaccard": j} for j, a, b in sorted(pairs, reverse=True)[:25]
        ]

        # repeated inline JSON blobs (the docs search index)
        blob: dict[str, list] = collections.defaultdict(list)
        for path, pg in self.pages.items():
            for elem_id, nbytes, line in pg.inline_json:
                blob[elem_id or "(anonymous)"].append((path, nbytes, line))
        self.stats["inline_json_blobs"] = {
            k: {
                "pages": len(v),
                "bytes_per_page": max(b for _, b, _ in v),
                "total_bytes": sum(b for _, b, _ in v),
                "in_visible_text": False,  # <script type=application/json> is not rendered text
            }
            for k, v in sorted(blob.items())
        }

    def check_markdown_twins(self):
        """Ensure article alternates exist without making .md files pages."""
        for rel, page in sorted(self.pages.items()):
            if not re.fullmatch(r"(?:answers|compare)/(?!index\.html$)[a-z0-9-]+\.html", rel):
                continue
            expected = "/" + rel[:-len(".html")] + ".md"
            twin = self.root / expected.lstrip("/")
            if not twin.is_file():
                self.add("markdown.missing", rel, 0,
                         f"Markdown twin {expected} is missing")
            alternates: list[str] = []
            head_match = re.search(r"<head\b[^>]*>(.*?)</head\s*>", self.raw[rel], re.I | re.S)
            head = head_match.group(1) if head_match else ""
            for tag in re.findall(r"<link\b[^>]*>", head, re.I):
                rel_match = re.search(r"\brel\s*=\s*[\"']([^\"']*)[\"']", tag, re.I)
                type_match = re.search(r"\btype\s*=\s*[\"']([^\"']*)[\"']", tag, re.I)
                href_match = re.search(r"\bhref\s*=\s*[\"']([^\"']*)[\"']", tag, re.I)
                rels = rel_match.group(1).lower().split() if rel_match else []
                if ("alternate" in rels and type_match
                        and type_match.group(1).lower() == "text/markdown"):
                    alternates.append(href_match.group(1) if href_match else "")
            if alternates != [expected]:
                self.add("markdown.alternate", rel, 0,
                         f"expected one text/markdown alternate href {expected!r}; got {alternates!r}")
            if re.search(r"<a\b[^>]*\bhref\s*=\s*[\"'](?![A-Za-z][A-Za-z0-9+.-]*:|//)[^\"']+\.md(?:[?#][^\"']*)?[\"']",
                         self.raw[rel], re.I):
                self.add("markdown.ordinary_link", rel, 0,
                         "Markdown twins must be referenced with <link rel=alternate>, not <a>")

    # 5. sitemap
    def check_sitemap(self):
        # normalise sitemap URLs -> files
        listed_files: dict[str, tuple[str, str | None]] = {}
        for loc, lastmod in self.sitemap_entries:
            parts = urllib.parse.urlsplit(loc)
            if parts.path.lower().endswith(".md"):
                self.add("sitemap.markdown", "sitemap.xml", 0,
                         f"{loc} is a Markdown twin; twins must stay out of sitemap.xml",
                         {"loc": loc})
                continue
            target = self.resolve_url_path(parts.path or "/")
            if target is None:
                self.add("sitemap.dead_url", "sitemap.xml", 0,
                         f"{loc} resolves to no file on disk", {"loc": loc})
                continue
            rp = self.rel(target)
            listed_files[rp] = (loc, lastmod)
            pg = self.pages.get(rp)
            if pg and loc != pg.canonical_url:
                self.add("sitemap.form_mismatch", "sitemap.xml", 0,
                         f"{loc} — canonical deployed form is {pg.canonical_url}",
                         {"loc": loc, "expected": pg.canonical_url, "file": rp})
            if lastmod:
                real = self.last_change_date(rp)
                if real and real != lastmod:
                    self.add("sitemap.stale_lastmod", "sitemap.xml", 0,
                             f"{loc}: lastmod {lastmod} but file last changed {real} "
                             f"({self.date_source})",
                             {"loc": loc, "lastmod": lastmod, "actual": real,
                              "source": self.date_source})

        for path, pg in sorted(self.pages.items()):
            if path in listed_files:
                continue
            if pg.robots and "noindex" in pg.robots:
                continue
            self.add("sitemap.missing_page", path, 0,
                     f"indexable page not listed in sitemap.xml ({pg.canonical_url})",
                     {"url": pg.canonical_url, "words": pg.word_count})
        self.stats["sitemap_urls"] = len(self.sitemap_entries)
        self.stats["sitemap_files_matched"] = len(listed_files)
        self.stats["sitemap_html_pages"] = len([f for f in listed_files if f in self.pages])
        self.stats["sitemap_non_html"] = sorted(f for f in listed_files if f not in self.pages)
        self.stats["html_pages_total"] = len(self.pages)

    # 7b. _redirects sources that shadow a real asset
    def check_redirect_collisions(self):
        for src, dest in sorted(self.redirects.items()):
            if not src.startswith("/"):
                continue
            rel = src.lstrip("/")
            on_disk = self.root / rel
            hit = None
            if (on_disk / "index.html").is_file():
                hit = self.rel(on_disk / "index.html")
            elif on_disk.is_file():
                hit = self.rel(on_disk)
            elif (self.root / (rel + ".html")).is_file():
                hit = self.rel(self.root / (rel + ".html"))
            if hit:
                self.add("redirect.collision", "_redirects", 0,
                         f"{src} redirects to {dest} but {hit} exists on disk — "
                         f"that page may be unreachable at {src}",
                         {"source": src, "dest": dest, "shadowed_file": hit})

    # 11. robots
    def check_robots(self):
        sitemap_paths = {urllib.parse.urlsplit(loc).path for loc, _ in self.sitemap_entries}
        advertised = set()
        for loc, _ in self.sitemap_entries:
            t = self.resolve_url_path(urllib.parse.urlsplit(loc).path or "/")
            if t is not None:
                advertised.add(self.rel(t))
        for path, pg in self.pages.items():
            if pg.robots:
                if "noindex" in pg.robots:
                    cid = "robots.noindex" if path in advertised else "robots.noindex.ok"
                    self.add(cid, path, pg.meta_lines.get("robots", 0),
                             f"meta robots = {pg.robots!r}"
                             + ("" if path in advertised else " (not in sitemap)"),
                             {"content": pg.robots, "in_sitemap": path in advertised})
                if "nofollow" in pg.robots:
                    self.add("robots.nofollow", path, pg.meta_lines.get("robots", 0),
                             f"meta robots = {pg.robots!r}", {"content": pg.robots})
        for kind, value in self.robots_rules:
            if kind != "disallow":
                continue
            prefix = value.rstrip("*")
            hit = sorted(p for p in sitemap_paths if p.startswith(prefix))
            if hit:
                self.add("robots.conflict", "robots.txt", 0,
                         f"Disallow: {value} blocks {len(hit)} sitemap URL(s)",
                         {"disallow": value, "blocked": hit})
        self.stats["robots_rules"] = [{"type": k, "value": v} for k, v in self.robots_rules]

    # 12. weight
    def check_weight(self):
        third_party: collections.Counter = collections.Counter()
        for path, pg in self.pages.items():
            total = pg.size
            for href, line, in_head, blocking in pg.stylesheets:
                if href.startswith("http"):
                    host = urllib.parse.urlsplit(href).netloc
                    third_party[host] += 1
                    if blocking:
                        self.add("weight.render_blocking", path, line,
                                 f"render-blocking third-party stylesheet from {host}",
                                 {"href": href, "host": host})
                    continue
                t = self.resolve_url_path(href if href.startswith("/") else
                                          urllib.parse.urljoin("/" + path, href))
                if t is not None:
                    total += self.file_size(t)
            for src, line, in_head, blocking in pg.scripts:
                if src.startswith("http"):
                    host = urllib.parse.urlsplit(src).netloc
                    third_party[host] += 1
                    if blocking and in_head:
                        self.add("weight.render_blocking", path, line,
                                 f"render-blocking third-party script from {host}",
                                 {"src": src, "host": host})
                    continue
                t = self.resolve_url_path(src if src.startswith("/") else
                                          urllib.parse.urljoin("/" + path, src))
                if t is not None:
                    total += self.file_size(t)
            for img in pg.images:
                src = img["src"]
                if src and not src.startswith(("http", "data:")):
                    t = self.resolve_url_path(src if src.startswith("/") else
                                              urllib.parse.urljoin("/" + path, src))
                    if t is not None:
                        total += self.file_size(t)
            pg.asset_bytes = total
            if total > PAGE_WEIGHT_WARN:
                self.add("weight.page", path, 0,
                         f"{total // 1024} KB including first-order assets",
                         {"bytes": total, "html_bytes": pg.size})
        self.stats["third_party_hosts"] = dict(third_party.most_common())

    # ── output ──────────────────────────────────────────────────────────
    def counts(self) -> collections.Counter:
        return collections.Counter(f.severity for f in self.findings)

    def by_check(self) -> dict[str, list[Finding]]:
        out: dict[str, list[Finding]] = collections.defaultdict(list)
        for f in self.findings:
            out[f.check].append(f)
        return out

    def to_json(self) -> dict:
        return {
            "meta": {
                "root": str(self.root),
                "origin": self.origin,
                "pages": len(self.pages),
                "date_source": self.date_source,
                "notes": self.notes,
                "thresholds": {
                    "title_max": TITLE_MAX, "desc_max": DESC_MAX, "desc_min": DESC_MIN,
                    "thin_words": THIN_WORDS, "max_depth": MAX_DEPTH,
                    "image_max_bytes": IMAGE_MAX_BYTES,
                    "duplicate_jaccard": DUPLICATE_JACCARD,
                },
            },
            "summary": {
                "total_findings": len(self.findings),
                "by_severity": {s: self.counts().get(s, 0) for s in SEVERITIES},
                "by_check": {
                    cid: {
                        "severity": severity_of(cid),
                        "what": CHECKS[cid][1],
                        "findings": len(fs),
                        "pages": len({f.path for f in fs}),
                    }
                    for cid, fs in sorted(self.by_check().items(),
                                          key=lambda kv: (SEVERITIES.index(severity_of(kv[0])),
                                                          -len(kv[1])))
                },
            },
            "stats": self.stats,
            "pages": {
                path: {
                    "url": pg.canonical_url,
                    "html_bytes": pg.size,
                    "asset_bytes": pg.asset_bytes,
                    "title": pg.title,
                    "title_chars": len(pg.title) if pg.title else 0,
                    "description_chars": len(pg.description) if pg.description else 0,
                    "lang": pg.lang,
                    "viewport": pg.viewport,
                    "canonical": pg.canonicals[0][0] if pg.canonicals else None,
                    "og_tags": sorted(k for k in pg.metas if k.startswith("og:")),
                    "twitter_tags": sorted(k for k in pg.metas if k.startswith("twitter:")),
                    "jsonld_blocks": len(pg.jsonld),
                    "h1_count": sum(1 for h in pg.headings if h[0] == 1),
                    "headings": len(pg.headings),
                    "words": pg.word_count,
                    "unique_words": pg.unique_word_count,
                    "images": len(pg.images),
                    "inbound_links": pg.inbound,
                    "depth": pg.depth,
                    "robots": pg.robots,
                    "inline_json_bytes": sum(b for _, b, _ in pg.inline_json),
                }
                for path, pg in sorted(self.pages.items())
            },
            "findings": [f.as_dict() for f in self.findings],
        }

    def text_report(self, limit: int = 25) -> str:
        out: list[str] = []
        w = out.append
        c = self.counts()
        w("=" * 78)
        w(f"TECHNICAL SEO AUDIT — {self.origin}")
        w(f"{len(self.pages)} HTML pages under {self.root}")
        w("=" * 78)
        w("")
        w(f"{'SEVERITY':<12}{'FINDINGS':>10}")
        for s in SEVERITIES:
            w(f"{s.upper():<12}{c.get(s, 0):>10}")
        w(f"{'TOTAL':<12}{len(self.findings):>10}")
        w("")
        if self.notes:
            w("NOTES")
            for n in self.notes:
                w(f"  ! {n}")
            w("")
        w(f"{'CHECK':<26}{'SEV':<10}{'N':>6}{'PAGES':>7}  WHAT")
        w("-" * 78)
        for cid, fs in sorted(self.by_check().items(),
                              key=lambda kv: (SEVERITIES.index(severity_of(kv[0])), -len(kv[1]))):
            w(f"{cid:<26}{severity_of(cid):<10}{len(fs):>6}{len({f.path for f in fs}):>7}  "
              f"{CHECKS[cid][1]}")
        w("")
        w("DETAIL")
        w("=" * 78)
        for cid, fs in sorted(self.by_check().items(),
                              key=lambda kv: (SEVERITIES.index(severity_of(kv[0])), -len(kv[1]))):
            w("")
            w(f"[{severity_of(cid).upper()}] {cid} — {CHECKS[cid][1]}  ({len(fs)} findings)")
            for f in fs[:limit]:
                loc = f"{f.path}:{f.line}" if f.line else f.path
                w(f"  {loc}  {f.message}")
            if len(fs) > limit:
                w(f"  … and {len(fs) - limit} more (see the JSON output for the full list)")
        return "\n".join(out) + "\n"


# ── cli ─────────────────────────────────────────────────────────────────────


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description="Technical SEO audit for the static xerj.org site.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="Exit code: 0 when findings at or above --fail-on are <= --max-findings, "
               "else 1. Use --fail-on none to always exit 0 (measurement mode).",
    )
    default_root = pathlib.Path(__file__).resolve().parents[2] / "landing"
    ap.add_argument("--root", type=pathlib.Path, default=default_root,
                    help=f"site root to walk (default: {default_root})")
    ap.add_argument("--origin", default=SITE_ORIGIN, help="deployed origin")
    ap.add_argument("--json", type=pathlib.Path, help="write the JSON summary here")
    ap.add_argument("--report", type=pathlib.Path, help="write the text report here")
    ap.add_argument("--fail-on", choices=(*SEVERITIES, "none"), default="none",
                    help="minimum severity that counts toward the failure threshold")
    ap.add_argument("--max-findings", type=int, default=0,
                    help="allowed number of findings at/above --fail-on (default 0)")
    ap.add_argument("--limit", type=int, default=25,
                    help="max detail lines printed per check (default 25)")
    ap.add_argument("--quiet", action="store_true", help="do not print the text report")
    args = ap.parse_args(argv)

    if not args.root.is_dir():
        print(f"audit.py: {args.root} is not a directory", file=sys.stderr)
        return 2

    audit = Audit(args.root, args.origin)
    audit.load()
    audit.run()

    report = audit.text_report(limit=args.limit)
    if not args.quiet:
        sys.stdout.write(report)
    if args.report:
        args.report.parent.mkdir(parents=True, exist_ok=True)
        args.report.write_text(report, encoding="utf-8")
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(audit.to_json(), indent=2, ensure_ascii=False) + "\n",
                             encoding="utf-8")

    if args.fail_on == "none":
        return 0
    cutoff = SEVERITIES.index(args.fail_on)
    gating = sum(1 for f in audit.findings if SEVERITIES.index(f.severity) <= cutoff)
    if gating > args.max_findings:
        print(f"\naudit.py: {gating} findings at severity >= {args.fail_on} "
              f"(threshold {args.max_findings})", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
