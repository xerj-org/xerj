#!/usr/bin/env python3
"""The dated GitHub counts and the three newest posts on the home page.

The home page's "Built in the open" section shows counts that must never be
typed by hand: contributors, releases and binary downloads, read from the
GitHub API with the time they were read. The three post links under them come
from landing/blog/index.html, newest first.

    python3 scripts/seo/home_counts.py            # read the API, save it, rewrite the home page
    python3 scripts/seo/home_counts.py --offline  # rewrite from the saved counts (no network)
    python3 scripts/seo/home_counts.py --check    # exit 1 if the home page differs

Counts: contributors = the repository's contributors list (no anonymous
entries); releases = published releases whose tag starts with v1.; downloads
= asset downloads over those releases, not counting .sha256 checksum files.
Stars are left out on purpose. Standard library only.
"""
import argparse, datetime, html, json, os, re, subprocess, sys, urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HOME = ROOT / "landing" / "index.html"
BLOG = ROOT / "landing" / "blog" / "index.html"
SAVED = Path(__file__).resolve().parent / "home_counts.json"
API = "https://api.github.com/repos/xerj-org/xerj"

COUNTS_RE = re.compile(r"(<!-- home:counts-begin[^>]*-->\n).*?(<!-- home:counts-end -->)", re.S)
POSTS_RE = re.compile(r"(<!-- home:posts-begin[^>]*-->\n).*?(<!-- home:posts-end -->)", re.S)
CARD_RE = re.compile(r'<a class="bl-card" href="(/blog/[^"]+)">.*?<time datetime="([0-9-]+)">.*?'
                     r'<h2 class="bl-title">(.*?)</h2>', re.S)


def _token():
    tok = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if tok:
        return tok
    try:
        return subprocess.run(["gh", "auth", "token"], capture_output=True, text=True).stdout.strip()
    except OSError:
        return ""


def _get(url):
    req = urllib.request.Request(url, headers={"Accept": "application/vnd.github+json",
                                               "User-Agent": "xerj-home-counts"})
    tok = _token()
    if tok:
        req.add_header("Authorization", f"Bearer {tok}")
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r), r.headers.get("Link", "")


def live():
    _, link = _get(f"{API}/contributors?per_page=1&anon=false")
    m = re.search(r'[?&]page=(\d+)>; rel="last"', link)
    contributors = int(m.group(1)) if m else len(_get(f"{API}/contributors?per_page=100")[0])
    rels, page = [], 1
    while True:
        batch, _ = _get(f"{API}/releases?per_page=100&page={page}")
        rels += batch
        if len(batch) < 100:
            break
        page += 1
    v1 = [r for r in rels if r.get("tag_name", "").startswith("v1.") and not r.get("draft")]
    downloads = sum(a["download_count"] for r in v1 for a in r.get("assets", [])
                    if not a["name"].endswith(".sha256"))
    first = min(r["published_at"] for r in v1)[:10]
    now = datetime.datetime.now(datetime.timezone.utc).replace(microsecond=0)
    return {"contributors": contributors, "releases": len(v1), "first_release": first,
            "downloads": downloads, "source": API,
            "fetched_at": now.isoformat().replace("+00:00", "Z")}


def _date(iso):
    d = datetime.date.fromisoformat(iso[:10])
    return f"{d.day} {d.strftime('%B')} {d.year}"


def render_counts(c):
    tiles = [(f"{c['contributors']:,}", "contributors"),
             (f"{c['releases']:,}", f"releases since {_date(c['first_release'])}"),
             (f"{c['downloads']:,}", "engine binary downloads from GitHub releases")]
    out = [f'<p class="label">Built in the open · counts from the GitHub API, {c["fetched_at"][:10]}</p>',
           '<div class="g counts">']
    for n, label in tiles:
        out.append(f'  <div class="c4"><p class="count">{n}</p><p class="count-l">{html.escape(label)}</p></div>')
    out.append("</div>")
    return "\n".join(out) + "\n"


def render_posts():
    cards = CARD_RE.findall(BLOG.read_text(encoding="utf-8"))
    cards.sort(key=lambda c: c[1], reverse=True)
    out = ['<ul class="posts">']
    for href, day, title in cards[:3]:
        t = " ".join(re.sub(r"<[^>]+>", "", title).split())
        out.append(f'  <li><a href="{href}">{t}</a> <time datetime="{day}">{day}</time></li>')
    out.append("</ul>")
    return "\n".join(out) + "\n"


def apply(text, c):
    text, n1 = COUNTS_RE.subn(lambda m: m.group(1) + render_counts(c) + m.group(2), text)
    text, n2 = POSTS_RE.subn(lambda m: m.group(1) + render_posts() + m.group(2), text)
    if n1 != 1 or n2 != 1:
        raise SystemExit("home_counts: landing/index.html lacks the home:counts or home:posts markers")
    return text


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--offline", action="store_true")
    a = ap.parse_args()
    if a.check or a.offline:
        c = json.loads(SAVED.read_text(encoding="utf-8"))
    else:
        c = live()
        SAVED.write_text(json.dumps(c, indent=2) + "\n", encoding="utf-8")
    old = HOME.read_text(encoding="utf-8")
    new = apply(old, c)
    if a.check:
        print(("FAIL home counts/posts are stale: run home_counts.py --offline" if new != old
               else f"ok   home counts ({c['fetched_at'][:10]}) and posts are current"))
        return 1 if new != old else 0
    if new != old:
        HOME.write_text(new, encoding="utf-8")
    print(f"home_counts: {c['contributors']} contributors, {c['releases']} releases, "
          f"{c['downloads']:,} downloads ({c['fetched_at']})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
