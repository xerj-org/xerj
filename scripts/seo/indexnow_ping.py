#!/usr/bin/env python3
"""Tell IndexNow search engines (Bing, Yandex, Seznam, Naver and others) which
xerj.org or hub.xerj.org URLs changed.

Run it by hand after a production deploy. No workflow calls it.

    # Print what would be sent (the default: nothing leaves the machine):
    python3 scripts/seo/indexnow_ping.py

    # Send every URL in the sitemap (local artifact if present, else the live one):
    python3 scripts/seo/indexnow_ping.py --send

    # Send only some URLs:
    python3 scripts/seo/indexnow_ping.py --send https://xerj.org/ https://xerj.org/product

    # hub.xerj.org, from its live sitemap:
    python3 scripts/seo/indexnow_ping.py --host hub.xerj.org --send

The key is public by design. IndexNow checks ownership by fetching
https://<host>/<key>.txt, so that file must be live on the host before you
send. For xerj.org it is landing/<key>.txt; for hub.xerj.org the hub
generator writes the same file into its out/ directory.

Only URLs on --host are sent. IndexNow accepts up to 10,000 URLs per request;
this script sends batches of 1,000.
"""
from __future__ import annotations

import argparse
import json
import sys
import urllib.error
import urllib.request
import xml.etree.ElementTree as ET
from pathlib import Path
from urllib.parse import urlparse

KEY = "e94e889e7bee019e40a7617324e7a20a"
ENDPOINT = "https://api.indexnow.org/indexnow"
ROOT = Path(__file__).resolve().parents[2]
LOCAL_SITEMAP = {"xerj.org": ROOT / "landing" / "sitemap.xml"}
BATCH = 1000
NS = "{http://www.sitemaps.org/schemas/sitemap/0.9}"


def sitemap_urls(host: str) -> list[str]:
    path = LOCAL_SITEMAP.get(host)
    if path and path.exists():
        data = path.read_bytes()
    else:
        with urllib.request.urlopen(f"https://{host}/sitemap.xml", timeout=30) as r:
            data = r.read()
    root = ET.fromstring(data)
    if root.tag == f"{NS}sitemapindex":
        out: list[str] = []
        for loc in root.iter(f"{NS}loc"):
            with urllib.request.urlopen(loc.text.strip(), timeout=30) as r:
                sub = ET.fromstring(r.read())
            out += [l.text.strip() for l in sub.iter(f"{NS}loc")]
        return out
    return [l.text.strip() for l in root.iter(f"{NS}loc")]


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("urls", nargs="*", help="URLs to send (default: every URL in the host's sitemap)")
    ap.add_argument("--host", default="xerj.org", choices=["xerj.org", "hub.xerj.org"])
    ap.add_argument("--send", action="store_true", help="actually POST to IndexNow (default is a dry run)")
    args = ap.parse_args(argv)

    urls = args.urls or sitemap_urls(args.host)
    urls = [u for u in dict.fromkeys(urls) if urlparse(u).netloc == args.host]
    if not urls:
        print(f"indexnow: no URLs on {args.host}", file=sys.stderr)
        return 1
    key_location = f"https://{args.host}/{KEY}.txt"
    print(f"indexnow: {len(urls)} URL(s) on {args.host}, key file {key_location}")
    if not args.send:
        for u in urls[:20]:
            print("  " + u)
        if len(urls) > 20:
            print(f"  ... and {len(urls) - 20} more")
        print("indexnow: dry run, nothing sent. Add --send to submit.")
        return 0

    for i in range(0, len(urls), BATCH):
        body = json.dumps({"host": args.host, "key": KEY, "keyLocation": key_location,
                           "urlList": urls[i:i + BATCH]}).encode()
        req = urllib.request.Request(ENDPOINT, data=body, method="POST",
                                     headers={"Content-Type": "application/json; charset=utf-8"})
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                print(f"indexnow: batch {i // BATCH + 1}: HTTP {r.status}")
        except urllib.error.HTTPError as e:
            # 200/202 mean accepted. 403 means the key file is not live on the host;
            # 422 means a URL is not on the host; 429 means too many requests.
            print(f"indexnow: batch {i // BATCH + 1}: HTTP {e.code} {e.read()[:200]!r}", file=sys.stderr)
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
