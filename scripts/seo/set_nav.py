#!/usr/bin/env python3
"""Rewrite the primary nav (`<nav class="nav" …>…</nav>`) of landing pages to the
2026-10 information architecture (/workspace notes: new_ia.md, in the PR body).

    python3 scripts/seo/set_nav.py landing/index.html landing/docs/*.html   # named pages
    python3 scripts/seo/set_nav.py --check landing/index.html                # exit 1 if stale

Four primary items (Docs, Corpus Hub, Use cases, For agents), then a GitHub icon
and the DAY/NIGHT toggle. Benchmarks, blog, case studies, recipes, dashboards,
releases, X and Discord live in the hubs and the footer map.

Only the pages you name are touched: pages owned by in-flight PRs keep their nav
until those land, then one run over `landing/**/*.html` unifies the site.
Stdlib only, like the rest of scripts/seo/.
"""
from __future__ import annotations

import argparse
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
LANDING = ROOT / "landing"
NAV_RE = re.compile(r'<nav class="nav"[^>]*>.*?</nav>', re.S)

ITEMS = [  # (label, href, section prefixes that make it active)
    ("DOCS", "/docs/", ("docs/",)),
    ("CORPUS HUB ↗", "https://hub.xerj.org", ()),
    ("USE CASES", "/use-cases", ("use-cases", "case-studies")),
    ("FOR AGENTS", "/for-agents", ("for-agents", "agent-search/")),
]


GITHUB_ICON = (
    '<svg width="18" height="18" viewBox="0 0 16 16" aria-hidden="true" focusable="false">'
    '<path fill="currentColor" d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 '
    '0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 '
    '1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15'
    '-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82'
    '.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93'
    '-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"/></svg>'
)


def render(rel: str) -> str:
    lines = ['<nav class="nav" aria-label="Primary" id="top">',
             '  <a href="/" class="brand" aria-label="XERJ home">XERJ</a>']
    for label, href, prefixes in ITEMS:
        ext = ' rel="external"' if href.startswith("http") else ""
        cur = any(rel.startswith(p) for p in prefixes)
        attrs = ' class="active" aria-current="page"' if cur else ""
        lines.append(f'  <a href="{href}"{ext}{attrs}>{label}</a>')
    lines += [
        '  <span class="spacer"></span>',
        '  <a class="nav-gh" href="https://github.com/xerj-org/xerj" rel="external" '
        f'aria-label="XERJ on GitHub" title="XERJ on GitHub">{GITHUB_ICON}</a>',
        '  <span class="theme">',
        '    <button type="button" data-theme="day" class="active">DAY</button>',
        '    <span class="dash">·</span>',
        '    <button type="button" data-theme="night">NIGHT</button>',
        '  </span>',
        '</nav>',
    ]
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--check", action="store_true")
    ap.add_argument("files", nargs="+", type=pathlib.Path)
    a = ap.parse_args()
    stale = 0
    for f in a.files:
        f = f.resolve()
        rel = f.relative_to(LANDING).as_posix()
        src = f.read_text()
        m = NAV_RE.search(src)
        if not m:
            print(f"skip  {rel}: no <nav class=\"nav\">", file=sys.stderr)
            continue
        new = src[:m.start()] + render(rel) + src[m.end():]
        if new != src:
            stale += 1
            if a.check:
                print(f"stale {rel}")
            else:
                f.write_text(new)
                print(f"nav   {rel}")
    return 1 if (a.check and stale) else 0


if __name__ == "__main__":
    sys.exit(main())
