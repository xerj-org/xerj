#!/usr/bin/env python3
"""Rewrite the primary nav (`<nav class="nav" …>…</nav>`) of every landing page
from one definition, so the header menu cannot drift page by page.

    python3 scripts/seo/set_nav.py            # rewrite every page with a primary nav
    python3 scripts/seo/set_nav.py --check    # exit 1 if any page differs

Menu: PRODUCT, USE CASES, ANSWERS, BENCHMARKS, BLOG | FOR AGENTS, DOCS,
DASHBOARDS, HUB, DONATE, then the GitHub stars badge, X, Discord and the DAY/NIGHT
toggle. The brand mark links home, so there is no HOME item. Case studies,
industries and comparisons are grouped under USE CASES and ANSWERS (they mark
those items active) and are listed in the footer map; recipes live in the docs.

Carried over from each page's existing nav: `id="top"` and the release
download button (`a.nav-dl`, home page only), whose tag
`landing-constants-guard.sh` keeps in one place. The stars badge markup comes
from stars_badge.badge(). build_articles.py renders its nav with render().
Stdlib only, like the rest of scripts/seo/.
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
NAV_DL_RE = re.compile(r'<a class="nav-dl"[^>]*>.*?</a>', re.S)

# (label, href, landing-relative prefixes that mark the item active)
LEFT = [
    ("PRODUCT", "/product", ("product.html",)),
    ("USE CASES", "/use-cases", ("use-cases", "case-studies", "industries/", "solutions/")),
    ("ANSWERS", "/answers/", ("answers/", "compare/")),
    ("BENCHMARKS", "/benchmarks/", ("benchmarks/",)),
    ("BLOG", "/blog/", ("blog/",)),
]
RIGHT = [
    ("FOR AGENTS", "/for-agents", ("for-agents", "agent-search/")),
    ("DOCS", "/docs/", ("docs/",)),
    ("DASHBOARDS", "/playground", ("playground",)),
    ("HUB", "https://hub.xerj.org", ()),
    ("DONATE", "/donate", ("donate",)),
]
X_LINK = ('<a href="https://x.com/xerj_org" rel="external" title="XERJ on X" '
          'style="display:inline-flex;align-items:center"><svg width="15" height="15" viewBox="0 0 24 24" '
          'fill="currentColor" aria-hidden="true"><path d="M18.244 2.25h3.308l-7.227 8.26 8.502 11.24H16.17l'
          '-5.214-6.817L4.99 21.75H1.68l7.73-8.835L1.254 2.25H8.08l4.713 6.231zm-1.161 17.52h1.833L7.084 '
          '4.126H5.117z"/></svg></a>')
DISCORD_LINK = (
    '<a href="https://discord.com/invite/yetXFusXfs" rel="external" title="XERJ on Discord" '
    'style="display:inline-flex;align-items:center"><svg width="15" height="15" viewBox="0 0 24 24" '
    'fill="currentColor" aria-hidden="true"><path d="M20.317 4.37a19.79 19.79 0 00-4.885-1.515.074.074 0 '
    '00-.079.037c-.21.375-.444.864-.608 1.25a18.27 18.27 0 00-5.487 0 12.64 12.64 0 00-.617-1.25.077.077 0 '
    '00-.079-.037A19.74 19.74 0 003.677 4.37a.07.07 0 00-.032.027C.533 9.046-.32 13.58.099 18.057a.082.082 0 '
    '00.031.057 19.9 19.9 0 005.993 3.03.078.078 0 00.084-.028c.462-.63.874-1.295 1.226-1.994a.076.076 0 '
    '00-.041-.106 13.1 13.1 0 01-1.872-.892.077.077 0 01-.008-.128 10.2 10.2 0 00.372-.292.074.074 0 '
    '01.077-.01c3.928 1.793 8.18 1.793 12.062 0a.074.074 0 01.078.01c.12.098.246.198.373.292a.077.077 0 '
    '01-.006.127 12.3 12.3 0 01-1.873.892.077.077 0 00-.041.107c.36.698.772 1.362 1.225 1.993a.076.076 0 '
    '00.084.028 19.84 19.84 0 006.002-3.03.077.077 0 00.032-.054c.5-5.177-.838-9.674-3.549-13.66a.061.061 0 '
    '00-.031-.03zM8.02 15.33c-1.183 0-2.157-1.085-2.157-2.419 0-1.333.956-2.419 2.157-2.419 1.21 0 2.176 '
    '1.096 2.157 2.42 0 1.333-.956 2.418-2.157 2.418zm7.975 0c-1.183 0-2.157-1.085-2.157-2.419 0-1.333.955'
    '-2.419 2.157-2.419 1.21 0 2.176 1.096 2.157 2.42 0 1.333-.946 2.418-2.157 2.418z"/></svg></a>')


def _item(rel: str, label: str, href: str, prefixes: tuple[str, ...]) -> str:
    ext = ' rel="external"' if href.startswith("http") else ""
    cur = ' class="active"' if any(rel.startswith(p) for p in prefixes) else ""
    return f'  <a href="{href}"{ext}{cur}>{label}</a>'


def render(rel: str, old: str = "") -> str:
    """The primary nav for landing-relative path `rel`; `old` is the nav it replaces."""
    top = ' id="top"' if 'id="top"' in old.split(">", 1)[0] else ""
    lines = [f'<nav class="nav" aria-label="Primary"{top}>',
             '  <a href="/" class="brand">XERJ</a>']
    lines += [_item(rel, *i) for i in LEFT]
    lines.append('  <span class="spacer"></span>')
    lines += [_item(rel, *i) for i in RIGHT]
    lines += ["  " + stars_badge.badge(), "  " + X_LINK, "  " + DISCORD_LINK]
    dl = NAV_DL_RE.search(old)
    if dl:
        lines.append("  " + dl.group(0))
    lines += ['  <span class="theme">',
              '    <button type="button" data-theme="day" class="active">DAY</button>',
              '    <span class="dash">·</span>',
              '    <button type="button" data-theme="night">NIGHT</button>',
              '  </span>',
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
