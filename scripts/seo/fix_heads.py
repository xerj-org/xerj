#!/usr/bin/env python3
"""Idempotent `<head>` transformer for landing/*.html.

For every page it guarantees, in `<head>` and nowhere else:

  * exactly one self-referencing `<link rel="canonical">`, absolute and
    extensionless, equal to `urlmap.deployed_url(page)`;
  * the full Open Graph set — og:title, og:description, og:url, og:type,
    og:image (+ width/height/alt), og:site_name, og:locale;
  * twitter:card, twitter:title, twitter:description, twitter:image;
  * JSON-LD: Organization + WebSite sitewide, one page-body type
    (SoftwareApplication / CollectionPage / TechArticle / Article / WebPage),
    and BreadcrumbList on every page below the root;
  * the `<title>` and `<meta name="description">` from `pagedata.py`;
  * `<meta name="robots" content="noindex">` on, and only on, the pages in
    `urlmap.NOINDEX`;
  * `lang` on `<html>` and a viewport meta, if the page was missing them.

Deliberately NOT emitted, per scripts' research doc §5:
  * FAQPage — Google restricted FAQ rich results to government/health sites,
    and they end entirely on 7 May 2026.
  * HowTo — the rich result and its documentation were removed outright.
  * aggregateRating / Review — we have no review corpus; inventing one is a
    manual-action risk under Google's structured-data spam policies.

## Idempotency

The transform is defined as: *delete every tag this script owns, then emit
them all from `pagedata.py` + `urlmap.py`*.  Because the delete step also
removes tags a previous run wrote, `f(f(x)) == f(x)` by construction rather
than by luck — there is no "already applied?" heuristic to get wrong.

    python3 scripts/seo/fix_heads.py --write
    python3 scripts/seo/fix_heads.py --check      # exit 1 if any file changes

CSP note: `<script type="application/ld+json">` is a *data block* per WHATWG
HTML ("prepare the script element" returns at step 13, before the CSP inline
check at step 21).  It is never an inline script, so `script-src` never
applies.  Do not touch `landing/_headers`; `seo_lint.py` fails the build if
anyone widens `script-src` on account of this file.
"""

from __future__ import annotations

import argparse
import html
import pathlib
import re
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import pagedata  # noqa: E402
import seo_head  # noqa: E402
import urlmap  # noqa: E402

BEGIN = seo_head.BEGIN
END = seo_head.END

# ── regexes for the tags this script owns ───────────────────────────────────

RE_BLOCK = re.compile(re.escape(BEGIN) + r".*?" + re.escape(END) + r"\n?", re.S)
RE_CANONICAL = re.compile(
    r"[ \t]*<link\b[^>]*\brel\s*=\s*[\"']?canonical[\"']?[^>]*>[ \t]*\n?", re.I)
RE_SOCIAL_META = re.compile(
    r"[ \t]*<meta\b[^>]*\b(?:property|name)\s*=\s*"
    r"[\"'](?:og:[^\"']*|twitter:[^\"']*|robots)[\"'][^>]*>[ \t]*\n?", re.I)
RE_JSONLD = re.compile(
    r"[ \t]*<script\b[^>]*\btype\s*=\s*[\"']application/ld\+json[\"'][^>]*>"
    r".*?</script>[ \t]*\n?", re.I | re.S)
RE_TITLE = re.compile(r"(<title\b[^>]*>)(.*?)(</title>)", re.I | re.S)
RE_DESC = re.compile(
    r"[ \t]*<meta\b[^>]*\bname\s*=\s*[\"']description[\"'][^>]*>", re.I)
RE_VIEWPORT = re.compile(
    r"<meta\b[^>]*\bname\s*=\s*[\"']viewport[\"']", re.I)
RE_HEAD = re.compile(r"(<head\b[^>]*>)(.*?)(</head\s*>)", re.I | re.S)
RE_HTML_TAG = re.compile(r"<html\b([^>]*)>", re.I)


def esc(s: str) -> str:
    return seo_head.esc(s)


def text_esc(s: str) -> str:
    return seo_head.text_esc(s)


def build_block(rel: str, title: str, desc: str, published: str,
                modified: str, meta: dict | None = None) -> str:
    return seo_head.build_block(rel, title, desc, published, modified, meta)


# ── the transform ───────────────────────────────────────────────────────────


def transform(rel: str, src: str, dates: urlmap.DateSource,
              path: pathlib.Path) -> str:
    meta = pagedata.entry(rel)

    m = RE_HEAD.search(src)
    if not m:
        raise ValueError(f"{rel}: no <head> found")
    open_tag, head, close_tag = m.groups()

    # 1. delete everything this script owns (including a previous run's block)
    head = RE_BLOCK.sub("", head)
    head = RE_CANONICAL.sub("", head)
    head = RE_SOCIAL_META.sub("", head)
    head = RE_JSONLD.sub("", head)

    # 2. title
    tm = RE_TITLE.search(head)
    if not tm:
        raise ValueError(f"{rel}: no <title>")
    existing_title = html.unescape(tm.group(2)).strip()
    title = meta.get("title", existing_title)
    head = head[:tm.start()] + tm.group(1) + text_esc(title) + tm.group(3) + head[tm.end():]

    # 3. description
    dm = RE_DESC.search(head)
    existing_desc = ""
    if dm:
        cm = re.search(r"\bcontent\s*=\s*([\"'])(.*?)\1", dm.group(0), re.S)
        existing_desc = html.unescape(cm.group(2)).strip() if cm else ""
    desc = meta.get("description", existing_desc)
    if not desc:
        raise ValueError(f"{rel}: no description in the file or in pagedata.py")
    desc_tag = f'<meta name="description" content="{esc(desc)}">'
    if dm:
        head = head[:dm.start()] + desc_tag + head[dm.end():]
    else:
        tm = RE_TITLE.search(head)
        head = head[:tm.end()] + "\n" + desc_tag + head[tm.end():]

    # 4. viewport (only aise-demo.html was missing one)
    if not RE_VIEWPORT.search(head):
        tm = RE_TITLE.search(head)
        vp = '<meta name="viewport" content="width=device-width, initial-scale=1.0">'
        head = head[:tm.start()] + vp + "\n" + head[tm.start():]

    # 5. emit the block immediately before </head>
    source_path = pathlib.Path(meta["_source_path"]) if meta.get("_source_path") else path
    published = meta.get("published") or dates.first_published(path)
    modified = meta.get("updated") or dates.last_modified(source_path)
    block = build_block(rel, title, desc, published, modified, meta)
    head = head.rstrip("\n \t") + "\n" + block + "\n"

    out = src[:m.start()] + open_tag + head + close_tag + src[m.end():]

    # 6. <html lang>
    hm = RE_HTML_TAG.search(out)
    if hm and not re.search(r"\blang\s*=", hm.group(1), re.I):
        attrs = hm.group(1)
        out = out[:hm.start()] + f'<html lang="en"{attrs}>' + out[hm.end():]

    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--write", action="store_true", help="rewrite files in place")
    ap.add_argument("--check", action="store_true",
                    help="exit 1 if any file would change (CI gate)")
    ap.add_argument("--root", default=None, help="landing directory")
    args = ap.parse_args(argv)
    if not (args.write or args.check):
        ap.print_help()
        return 2

    root = pathlib.Path(args.root) if args.root else urlmap.landing_dir()
    dates = urlmap.DateSource(urlmap.repo_root())
    if not dates.git_ok:
        print("WARNING: git unusable — dates fall back to filesystem mtime",
              file=sys.stderr)

    changed: list[str] = []
    for rel in urlmap.iter_pages(root):
        path = root / rel
        src = path.read_text(encoding="utf-8")
        out = transform(rel, src, dates, path)
        if out != src:
            changed.append(rel)
            if args.write:
                path.write_text(out, encoding="utf-8")

    if dates.fallbacks:
        print(f"WARNING: mtime fallback used for {len(dates.fallbacks)} file(s): "
              + ", ".join(dates.fallbacks[:5])
              + (" ..." if len(dates.fallbacks) > 5 else ""), file=sys.stderr)

    if args.check:
        if changed:
            print(f"FAIL {len(changed)} page(s) differ from the generated head:",
                  file=sys.stderr)
            for rel in changed[:20]:
                print(f"  {rel}", file=sys.stderr)
            return 1
        print("ok   every <head> matches the generator")
        return 0

    print(f"rewrote {len(changed)} of {len(urlmap.iter_pages(root))} pages")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
