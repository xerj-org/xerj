#!/usr/bin/env python3
"""Post-deploy check: every sitemap URL is really served, as itself.

**This is not a CI gate and must never become one.**  `.github/workflows/
seo.yml` runs against the committed tree on a pull request, before anything
is deployed; every URL this script asks for would 404 there, and a gate that
fails on unmerged work is a gate people learn to ignore.  `seo_lint.py`'s
header says the same thing from the other side: the rules that need the live
site "belong in a post-deploy job, not the merge gate".  Run this **after**
a deploy, by hand or from a job that triggers on a successful deployment.

Why it exists
-------------
On 2026-08-22 xerj.org answered **every** unknown path with the homepage and
an HTTP 200::

    /                                200  21841 B  ETag "93477f59…"
    /answers/what-is-xerj            200  21841 B  ETag "93477f59…"
    /this-page-has-never-existed…    200  21841 B  ETag "93477f59…"

Cloudflare Pages publishes `landing/` verbatim and, with no top-level
`404.html`, "assumes that you are deploying a single-page application" and
serves the root `index.html` for anything it cannot match.  Three URLs, one
of which had never existed, came back as one body with one ETag.

Every merge gate in `scripts/seo/` passed throughout, because all of them
reason about files that exist.  Nothing that reads only the repository can
see this class of defect: the tree was correct and the deployment was not.
That is the gap this script closes, and it is why the *duplicate-of-the-
homepage* check below is not a nicety — it is the exact signature of the bug.

The three checks (`seo_lint.py` reserves rules 15, 22 and 63 for the live
site; this file is where they are implemented and, for the first time,
written down):

    rule 15  every `<loc>` in the committed sitemap returns a **literal**
             200 — redirects are followed by nobody here, because a `<loc>`
             that 3xx-redirects is the wrong spelling of the URL and
             `urlmap.deployed_url` exists to prevent exactly that;
    rule 22  the served page's `<link rel="canonical">` is the URL that was
             requested — a canonical pointing somewhere else means the page
             is telling Google to index a different document;
    rule 63  the body is **not** byte-identical to the homepage — the soft-404
             signature above, and equally the signature of a rewrite rule
             that quietly swallowed a path.

Plus one probe that needs no sitemap: a path that has never existed must
answer **404**, not 200.  That single request is the fastest way to tell
whether the not-found handling is still correct after any deploy.

The sitemap comes from the **committed** `landing/sitemap.xml`, never from
`<base>/sitemap.xml`.  The question being asked is "did what we committed
actually reach production", and reading the deployed sitemap would let a
stale or truncated deploy answer for itself.

Usage
-----
    python3 scripts/seo/verify_live.py                       # https://xerj.org
    python3 scripts/seo/verify_live.py https://staging.example.com
    python3 scripts/seo/verify_live.py --jobs 4 --json
    python3 scripts/seo/verify_live.py --limit 20            # smoke test

Exit codes
    0   every URL passed
    1   at least one URL failed a check
    2   the run could not be made (no sitemap, unreadable, bad --base)

Dependencies: Python 3 standard library only — `urllib`, like every other
tool in this directory.  No requests, no npm, no lockfile.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import pathlib
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
import xml.etree.ElementTree as ET

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import urlmap  # noqa: E402

SITEMAP_NS = "{http://www.sitemaps.org/schemas/sitemap/0.9}"

#: A path that has never been a page on this site and never will be.  The
#: suffix is fixed rather than random so the request is cacheable and two runs
#: are comparable; it is long enough that it cannot collide with a real slug.
NOT_FOUND_PROBE = "/this-path-has-never-existed-verify-live-probe"

#: Cloudflare fronts this site and answers a bare `urllib` User-Agent with a
#: 403 challenge, which would look like a site failure and is not one.  A
#: normal browser UA plus an identifying comment is the honest form: it says
#: who is asking without pretending the request is a human's.
USER_AGENT = (
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) "
    "Chrome/128.0.0.0 Safari/537.36 (+xerj-verify-live; scripts/seo/verify_live.py)"
)

_CANONICAL_RE = re.compile(
    r"""<link\b[^>]*\brel\s*=\s*["']?canonical["']?[^>]*>""", re.I)
_HREF_RE = re.compile(r"""\bhref\s*=\s*["']([^"']*)["']""", re.I)

DEFAULT_TIMEOUT = 20


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """Report the redirect instead of following it.

    A `<loc>` that redirects is a defect even when the destination is fine:
    it costs every crawler an extra round trip and splits the link graph
    across two spellings of one document.  Following it would hide that.
    """

    def redirect_request(self, req, fp, code, msg, headers, newurl):  # noqa: D102
        return None


_OPENER = urllib.request.build_opener(_NoRedirect)


class Result:
    __slots__ = ("url", "status", "size", "sha", "canonical", "failures", "error")

    def __init__(self, url: str):
        self.url = url
        self.status: int | None = None
        self.size: int | None = None
        self.sha: str | None = None
        self.canonical: str | None = None
        self.failures: list[str] = []
        self.error: str | None = None

    @property
    def ok(self) -> bool:
        return not self.failures and self.error is None

    def as_dict(self) -> dict:
        return {
            "url": self.url, "status": self.status, "size": self.size,
            "sha256": self.sha, "canonical": self.canonical,
            "failures": self.failures, "error": self.error, "ok": self.ok,
        }


def _fetch(url: str, timeout: int) -> tuple[int, bytes, str | None]:
    """Return (status, body, error).  Never raises on an HTTP status."""
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with _OPENER.open(req, timeout=timeout) as r:
            return r.status, r.read(), None
    except urllib.error.HTTPError as e:
        # A 3xx reaches here because _NoRedirect refuses to follow it; the
        # Location header is the useful part of the report.
        body = e.read()
        loc = e.headers.get("Location")
        if loc and 300 <= e.code < 400:
            return e.code, body, f"redirects to {loc}"
        return e.code, body, None
    except Exception as e:                     # noqa: BLE001 — network, DNS, TLS
        return 0, b"", f"{type(e).__name__}: {e}"


def _canonical_of(body: bytes) -> str | None:
    try:
        text = body.decode("utf-8", "replace")
    except Exception:                          # noqa: BLE001
        return None
    m = _CANONICAL_RE.search(text)
    if not m:
        return None
    h = _HREF_RE.search(m.group(0))
    return h.group(1).strip() if h else None


def sitemap_urls(sitemap: pathlib.Path) -> list[str]:
    """Every `<loc>` in the committed sitemap, in document order."""
    root = ET.parse(sitemap).getroot()
    out = []
    for u in root.findall(f"{SITEMAP_NS}url"):
        loc = (u.findtext(f"{SITEMAP_NS}loc") or "").strip()
        if loc:
            out.append(loc)
    return out


def rebase(loc: str, base: str) -> str:
    """Point a canonical `<loc>` at the deployment under test."""
    p = urllib.parse.urlsplit(loc)
    return urllib.parse.urlunsplit(
        urllib.parse.urlsplit(base)[:2] + (p.path, p.query, p.fragment))


def check_one(url: str, home_sha: str | None, timeout: int) -> Result:
    res = Result(url)
    status, body, err = _fetch(url, timeout)
    res.status, res.size = status, len(body)
    res.sha = hashlib.sha256(body).hexdigest() if body else None
    if err and status == 0:
        res.error = err
        return res
    if status != 200:
        res.failures.append(
            f"HTTP {status}" + (f" ({err})" if err else "") + " — expected a literal 200 (rule 15)")
        return res
    res.canonical = _canonical_of(body)
    if home_sha and res.sha == home_sha:
        res.failures.append(
            "body is byte-identical to the homepage — this URL is not being "
            "served as itself (rule 63)")
    if res.canonical is None:
        res.failures.append("served page has no <link rel=canonical> (rule 22)")
    elif res.canonical.rstrip("/") != url.rstrip("/"):
        res.failures.append(
            f"canonical {res.canonical} != requested {url} (rule 22)")
    return res


def probe_not_found(base: str, timeout: int) -> Result:
    url = base.rstrip("/") + NOT_FOUND_PROBE
    res = Result(url)
    status, body, err = _fetch(url, timeout)
    res.status, res.size = status, len(body)
    res.sha = hashlib.sha256(body).hexdigest() if body else None
    if status == 0:
        res.error = err
        return res
    if status != 404:
        res.failures.append(
            f"a path that has never existed answered HTTP {status}, not 404 — "
            "the site is soft-404ing every unknown URL")
    return res


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(
        description=__doc__.splitlines()[0],
        epilog="Post-deploy tool. Not a CI gate: it needs a live deployment.")
    ap.add_argument("base", nargs="?", default=urlmap.SITE_ORIGIN,
                    help=f"origin to test (default {urlmap.SITE_ORIGIN})")
    ap.add_argument("--sitemap", default=None,
                    help="committed sitemap (default landing/sitemap.xml)")
    ap.add_argument("--jobs", type=int, default=8, help="parallel requests (default 8)")
    ap.add_argument("--timeout", type=int, default=DEFAULT_TIMEOUT)
    ap.add_argument("--limit", type=int, default=0, help="test only the first N URLs")
    ap.add_argument("--json", action="store_true", help="machine-readable report")
    ap.add_argument("--skip-not-found-probe", action="store_true")
    args = ap.parse_args(argv)

    base = args.base.rstrip("/")
    if not urllib.parse.urlsplit(base).scheme:
        print(f"verify_live: --base {args.base!r} has no scheme", file=sys.stderr)
        return 2

    sitemap = (pathlib.Path(args.sitemap) if args.sitemap
               else urlmap.landing_dir() / "sitemap.xml")
    if not sitemap.is_file():
        print(f"verify_live: no sitemap at {sitemap}", file=sys.stderr)
        return 2
    try:
        locs = sitemap_urls(sitemap)
    except ET.ParseError as e:
        print(f"verify_live: {sitemap} does not parse: {e}", file=sys.stderr)
        return 2
    if not locs:
        print(f"verify_live: {sitemap} lists no URLs", file=sys.stderr)
        return 2

    urls = [rebase(loc, base) for loc in locs]
    if args.limit:
        urls = urls[:args.limit]

    # The homepage is fetched first and alone: every other check compares
    # against it, so a run cannot start until it is known.
    home = check_one(base + "/", None, args.timeout)
    home_sha = home.sha if home.status == 200 else None
    if home_sha is None and not args.json:
        print(f"WARNING: {base}/ did not return 200 — the duplicate-of-homepage "
              f"check (rule 63) is disabled for this run", file=sys.stderr)

    results: list[Result] = []
    with concurrent.futures.ThreadPoolExecutor(max_workers=max(1, args.jobs)) as ex:
        futures = {ex.submit(check_one, u, home_sha, args.timeout): u
                   for u in urls if u.rstrip("/") != base}
        results.append(home)
        for f in concurrent.futures.as_completed(futures):
            results.append(f.result())
    results.sort(key=lambda r: r.url)

    probe = None if args.skip_not_found_probe else probe_not_found(base, args.timeout)

    bad = [r for r in results if not r.ok]
    if probe is not None and not probe.ok:
        bad.append(probe)

    if args.json:
        print(json.dumps({
            "base": base, "sitemap": str(sitemap), "checked": len(results),
            "failed": len(bad),
            "not_found_probe": probe.as_dict() if probe else None,
            "results": [r.as_dict() for r in results],
        }, indent=2))
    else:
        for r in results:
            if r.ok:
                continue
            print(f"FAIL {r.url}")
            if r.error:
                print(f"       unreachable: {r.error}")
            for f in r.failures:
                print(f"       {f}")
        if probe is not None:
            verdict = "ok  " if probe.ok else "FAIL"
            print(f"{verdict} not-found probe {probe.url} -> "
                  f"{probe.status if probe.status else probe.error}")
            for f in probe.failures:
                print(f"       {f}")
        print(f"\nverify_live: {base} · {len(results)} URL(s) from {sitemap.name} · "
              f"{len(bad)} failure(s)")

    return 1 if bad else 0


if __name__ == "__main__":
    raise SystemExit(main())
