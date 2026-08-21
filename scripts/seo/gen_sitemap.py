#!/usr/bin/env python3
"""Regenerate landing/sitemap.xml from the filesystem.

The previous sitemap was hand-maintained: 24 entries for an 80-page site, 18
of which were URLs that 308-redirect rather than pages, four of which were
non-HTML assets (`llms.txt` and three tool-schema JSON files), all 24 carrying
a `<priority>` and all 24 carrying a `<lastmod>` that disagreed with git.

This script replaces that with a derivation:

  * one `<url>` per HTML file under `landing/`, minus `urlmap.SITEMAP_EXCLUDE`;
  * `<loc>` = `urlmap.deployed_url(file)` — absolute, https, extensionless,
    byte-identical to that page's `<link rel="canonical">` and `og:url`;
  * `<lastmod>` = `git log -1 --format=%cs -- <file>`, i.e. the file's real
    last commit date, so the value is verifiable by construction.  Google only
    uses lastmod "if it's consistently and verifiably ... accurate"; a wrong
    lastmod is worse than none because it burns the field's credibility
    sitewide.  When git cannot answer, the filesystem mtime is used and the
    substitution is printed loudly on stderr.
  * **no `<priority>`, no `<changefreq>`.**  Google: "Google ignores
    <priority> and <changefreq> values."  Bing (July 2025): both "are ignored
    by Bing and do not influence how your content is crawled or ranked."
    They are pure noise, so they are not emitted.
  * no non-HTML assets.  A `.txt` or `.json` URL produces no search result;
    `llms.txt` stays advertised in `robots.txt` where it belongs.

One flat sitemap, no index file: at 78 URLs we are three orders of magnitude
under the 50,000-URL / 50 MB limit and an index would only add failure modes.

    python3 scripts/seo/gen_sitemap.py --write
    python3 scripts/seo/gen_sitemap.py --check   # exit 1 if committed != generated
"""

from __future__ import annotations

import argparse
import datetime as dt
import pathlib
import sys
import xml.sax.saxutils as saxutils
import urllib.parse

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import urlmap  # noqa: E402

SITEMAP_NS = "http://www.sitemaps.org/schemas/sitemap/0.9"
MAX_URLS = 50_000
MAX_BYTES = 50_000_000


def build(root: pathlib.Path, dates: urlmap.DateSource) -> str:
    rows: list[tuple[str, str]] = []
    for rel in urlmap.iter_pages(root):
        if not urlmap.in_sitemap(rel):
            continue
        rows.append((urlmap.deployed_url(rel), dates.last_modified(root / rel)))
    rows.sort()

    today = dt.date.today().isoformat()
    out = ['<?xml version="1.0" encoding="UTF-8"?>',
           f'<urlset xmlns="{SITEMAP_NS}">']
    for loc, lastmod in rows:
        if lastmod > today:                    # never advertise the future
            lastmod = today
        out.append("  <url>")
        # All data values must use entity escape codes (sitemaps.org); a raw
        # '&' in a URL makes the whole file malformed and silently unusable.
        out.append(f"    <loc>{saxutils.escape(loc)}</loc>")
        out.append(f"    <lastmod>{lastmod}</lastmod>")
        out.append("  </url>")
    out.append("</urlset>")
    return "\n".join(out) + "\n"


def validate(xml: str) -> list[str]:
    problems = []
    data = xml.encode("utf-8")
    if data.startswith(b"\xef\xbb\xbf"):
        problems.append("sitemap starts with a UTF-8 BOM")
    if len(data) > MAX_BYTES:
        problems.append(f"sitemap is {len(data)} bytes (limit {MAX_BYTES})")
    n = xml.count("<loc>")
    if n > MAX_URLS:
        problems.append(f"sitemap has {n} URLs (limit {MAX_URLS})")
    for dead in ("<priority>", "<changefreq>"):
        if dead in xml:
            problems.append(f"{dead} present — ignored by Google and Bing")
    for loc in _locs(xml):
        if urllib.parse.urlsplit(loc).path.lower().endswith(".md"):
            problems.append(f"Markdown twin {loc} must not appear in sitemap.xml")
    return problems


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--write", action="store_true", help="write landing/sitemap.xml")
    ap.add_argument("--check", action="store_true",
                    help="exit 1 if the committed sitemap differs (CI gate)")
    ap.add_argument("--root", default=None)
    args = ap.parse_args(argv)
    if not (args.write or args.check):
        ap.print_help()
        return 2

    root = pathlib.Path(args.root) if args.root else urlmap.landing_dir()
    dates = urlmap.DateSource(urlmap.repo_root())
    if not dates.git_ok:
        print("WARNING: git is unusable here — every <lastmod> below falls back "
              "to the filesystem mtime, which Google will not be able to verify",
              file=sys.stderr)

    xml = build(root, dates)

    if dates.fallbacks:
        print(f"WARNING: <lastmod> fell back to filesystem mtime for "
              f"{len(dates.fallbacks)} file(s):", file=sys.stderr)
        for p in dates.fallbacks:
            print(f"  mtime-fallback {p}", file=sys.stderr)

    problems = validate(xml)
    if problems:
        for p in problems:
            print(f"FAIL {p}", file=sys.stderr)
        return 1

    out = root / "sitemap.xml"
    if args.check:
        current = out.read_text(encoding="utf-8") if out.exists() else ""
        if current != xml:
            print("FAIL landing/sitemap.xml is out of date — run "
                  "`python3 scripts/seo/gen_sitemap.py --write`", file=sys.stderr)
            cur_locs = set(_locs(current))
            new_locs = set(_locs(xml))
            for loc in sorted(new_locs - cur_locs)[:10]:
                print(f"  + {loc}", file=sys.stderr)
            for loc in sorted(cur_locs - new_locs)[:10]:
                print(f"  - {loc}", file=sys.stderr)
            return 1
        print(f"ok   sitemap.xml matches ({xml.count('<loc>')} URLs)")
        return 0

    out.write_text(xml, encoding="utf-8")
    print(f"wrote {out} ({xml.count('<loc>')} URLs, {len(xml.encode())} bytes)")
    return 0


def _locs(xml: str) -> list[str]:
    import re
    return re.findall(r"<loc>(.*?)</loc>", xml)


if __name__ == "__main__":
    raise SystemExit(main())
