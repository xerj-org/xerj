#!/usr/bin/env python3
"""Rewrite the primary nav (`<nav class="nav" …>…</nav>`) of every landing
page from one definition, so the header menu cannot drift page by page.

    python3 scripts/seo/set_nav.py            # rewrite every page with a primary nav
    python3 scripts/seo/set_nav.py --check    # exit 1 if any page differs

Menu (2026-11-07 home redesign): Query-1, Use cases, Benchmarks, Docs, Blog,
Hub, then the GitHub stars badge. The brand mark links home. Everything else
(answers, comparisons, recipes, case studies, X, Discord, donate, the theme
toggle) lives in the generated footer (fix_links.py), so the nav is one row of
chrome. At 720 px and below the links fold into a <details> "Menu" that holds
the same links, so the nav needs no script.

Carried over from each page's existing nav: `id="top"`. The stars badge
markup comes from stars_badge.badge(). build_articles.py renders its nav with
render(). Stdlib only, like the rest of scripts/seo/.
"""
from __future__ import annotations

import argparse
import pathlib
import re
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import stars_badge  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
LANDING = ROOT / "landing"
NAV_RE = re.compile(r'<nav class="nav"[^>]*>.*?</nav>', re.S)

# (label, href, landing-relative prefixes that mark the item active)
LINKS = [
    ("Query-1", "/xerj-query-1", ("xerj-query-1",)),
    ("Use cases", "/#use-cases", ("use-cases", "case-studies", "integrations/")),
    ("Benchmarks", "/benchmarks/", ("benchmarks/",)),
    ("Docs", "/docs/", ("docs/",)),
    ("Blog", "/blog/", ("blog/",)),
    ("Hub", "https://hub.xerj.org", ()),
]
GITHUB = "https://github.com/xerj-org/xerj"


def _item(rel: str, label: str, href: str, prefixes: tuple[str, ...], pad: str = "  ") -> str:
    ext = ' rel="external"' if href.startswith("http") else ""
    cur = ' class="active" aria-current="page"' if any(rel.startswith(p) for p in prefixes) else ""
    return f'{pad}<a href="{href}"{ext}{cur}>{label}</a>'


def render(rel: str, old: str = "") -> str:
    """The primary nav for landing-relative path `rel`; `old` is the nav it replaces.

    One row: the brand, six links, the GitHub stars badge. At 720 px and below
    style.css hides the row of links and shows the <details> menu, which holds
    the same links (no script needed).
    """
    top = ' id="top"' if 'id="top"' in old.split(">", 1)[0] else ""
    lines = [f'<nav class="nav" aria-label="Primary"{top}>',
             '  <a href="/" class="brand">XERJ</a>',
             '  <span class="nav-links">']
    lines += [_item(rel, *i, pad="    ") for i in LINKS]
    lines += ['  </span>',
              '  <span class="spacer"></span>',
              "  " + stars_badge.badge(),
              '  <details class="nav-menu">',
              '    <summary>Menu</summary>',
              '    <span class="nav-menu-list">']
    lines += [_item(rel, *i, pad="      ") for i in LINKS]
    lines += [f'      <a href="{GITHUB}" rel="external">GitHub</a>',
              '    </span>',
              '  </details>',
              '</nav>']
    return "\n".join(lines)


def transform(rel: str, text: str) -> str:
    return NAV_RE.sub(lambda m: render(rel, m.group(0)), text, count=1)


def pages():
    for p in sorted(LANDING.rglob("*.html")):
        text = p.read_text(encoding="utf-8")
        if NAV_RE.search(text):
            yield p, p.relative_to(LANDING).as_posix(), text


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="exit 1 if any nav is stale")
    args = ap.parse_args(argv)
    stale = 0
    total = 0
    for p, rel, text in pages():
        total += 1
        new = transform(rel, text)
        if new != text:
            stale += 1
            if args.check:
                print(f"stale nav: landing/{rel}")
            else:
                p.write_text(new, encoding="utf-8")
    if args.check:
        print(f"{'FAIL' if stale else 'ok  '} {total} primary navs, {stale} stale")
        return 1 if stale else 0
    print(f"rewrote {stale} of {total} primary navs")
    return 0


if __name__ == "__main__":
    sys.exit(main())
