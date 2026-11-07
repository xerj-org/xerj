#!/usr/bin/env python3
"""The GitHub stars badge in the primary nav of every xerj.org page.

Every page with the primary nav (<nav class="nav" ...>) carries one badge that
links to the repository and shows its star count:

    <a class="nav-stars" href="https://github.com/xerj-org/xerj" ...>
      <svg>GitHub mark</svg><span class="nav-stars-n" data-gh-stars="14225">★ 14.2k</span></a>

The number in the HTML is a build-time fallback, read from the GitHub API when
this script runs and saved in scripts/seo/github_stars.json (with the time it
was read), so the badge is never empty. build_articles.py uses the same file. landing/theme.js refreshes it in
the browser from api.github.com, cached in localStorage for 10 minutes; if that
fails, the committed number stays.

    python3 scripts/seo/stars_badge.py            # fetch the live count, save it, rewrite every nav
    python3 scripts/seo/stars_badge.py --offline  # rewrite every nav from the saved count (no network)
    python3 scripts/seo/stars_badge.py --check    # every nav has one badge with the saved count

After a refresh, run build_articles.py so the generated article pages match.

The first run replaces the plain GITHUB text link in the nav with the badge;
later runs only update the number. Standard library only.
"""
import argparse, datetime, json, os, re, subprocess, sys, urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
LANDING = ROOT / "landing"
REPO = "xerj-org/xerj"
REPO_URL = f"https://github.com/{REPO}"
API = f"https://api.github.com/repos/{REPO}"
SAVED = Path(__file__).resolve().parent / "github_stars.json"

GH_MARK = ("M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49"
           "-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82"
           ".72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15"
           "-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82"
           " 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48"
           " 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z")

NAV_RE = re.compile(r'<nav class="nav"[^>]*>.*?</nav>', re.S)
PLAIN_LINK = f'<a href="{REPO_URL}" rel="external">GITHUB</a>'
BADGE_RE = re.compile(r'<a class="nav-stars"[^>]*>.*?</a>', re.S)
COUNT_RE = re.compile(r'data-gh-stars="(\d+)"')


def compact(n):
    """14225 -> '14.2k', 950 -> '950', 2000 -> '2k'. Same rounding as theme.js."""
    if n < 1000:
        return str(n)
    return f"{int(n / 100 + 0.5) / 10:.1f}".removesuffix(".0") + "k"


def saved_count():
    """The fallback count saved by the last live read."""
    return json.loads(SAVED.read_text(encoding="utf-8"))["stargazers_count"]


def saved_date():
    """The day the saved count was read, for the badge title."""
    return json.loads(SAVED.read_text(encoding="utf-8")).get("fetched_at", "")[:10]


def badge(n=None):
    n = saved_count() if n is None else n
    when = saved_date()
    title = f"XERJ on GitHub ({n:,} stars on {when} by the GitHub API)" if when else "XERJ on GitHub"
    return (f'<a class="nav-stars" href="{REPO_URL}" rel="external" title="{title}" '
            f'aria-label="XERJ on GitHub, {n:,} stars">'
            f'<svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true"><path d="{GH_MARK}"/></svg>'
            f'GitHub <span class="nav-stars-n" data-gh-stars="{n}">\u2605 {compact(n)}</span></a>')


def live_count():
    req = urllib.request.Request(API, headers={"Accept": "application/vnd.github+json",
                                               "User-Agent": "xerj-stars-badge"})
    tok = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if not tok:
        try:
            tok = subprocess.run(["gh", "auth", "token"], capture_output=True, text=True).stdout.strip()
        except OSError:
            tok = ""
    if tok:
        req.add_header("Authorization", f"Bearer {tok}")
    with urllib.request.urlopen(req, timeout=20) as r:
        n = json.load(r)["stargazers_count"]
    if not isinstance(n, int) or n <= 0:
        raise SystemExit(f"stars_badge: unexpected stargazers_count {n!r}")
    return n


def pages():
    return sorted(p for p in LANDING.rglob("*.html") if NAV_RE.search(p.read_text(encoding="utf-8")))


def apply(text, n):
    def nav(m):
        block = m.group(0)
        if BADGE_RE.search(block):
            return BADGE_RE.sub(lambda _: badge(n), block, count=1)
        if PLAIN_LINK in block:
            return block.replace(PLAIN_LINK, badge(n), 1)
        raise ValueError("primary nav has neither the GITHUB link nor a stars badge")
    return NAV_RE.sub(nav, text, count=1)


def check():
    bad, counts = [], set()
    for p in pages():
        block = NAV_RE.search(p.read_text(encoding="utf-8")).group(0)
        found = BADGE_RE.findall(block)
        rel = p.relative_to(ROOT)
        if len(found) != 1:
            bad.append(f"{rel}: {len(found)} stars badge(s) in the primary nav (want 1)")
            continue
        m = COUNT_RE.search(found[0])
        if not m or found[0] != badge(int(m.group(1))):
            bad.append(f"{rel}: stars badge markup differs from stars_badge.badge()")
            continue
        counts.add(int(m.group(1)))
    want = saved_count()
    if counts - {want}:
        bad.append(f"pages carry {sorted(counts)} but {SAVED.name} says {want}: run stars_badge.py --offline")
    for b in bad:
        print("FAIL", b)
    if not bad:
        print(f"ok   {len(pages())} navs carry the stars badge (fallback {counts.pop() if counts else '?'})")
    return 1 if bad else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--offline", action="store_true")
    a = ap.parse_args()
    if a.check:
        return check()
    if a.offline:
        n = saved_count()
    else:
        n = live_count()
        now = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")
        SAVED.write_text(json.dumps({"repo": REPO, "stargazers_count": n, "source": API,
                                     "fetched_at": now}, indent=2) + "\n", encoding="utf-8")
    changed = 0
    for p in pages():
        old = p.read_text(encoding="utf-8")
        new = apply(old, n)
        if new != old:
            p.write_text(new, encoding="utf-8")
            changed += 1
    print(f"stars_badge: {n:,} stars ({compact(n)}), {changed} page(s) updated")
    return 0


if __name__ == "__main__":
    sys.exit(main())
