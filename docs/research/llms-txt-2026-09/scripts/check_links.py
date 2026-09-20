#!/usr/bin/env python3
"""Fetch every URL printed in the four proposal files and record what answered.

The proposals may only link to pages that exist, with two declared exceptions that the ship
checklist (report.md §9) is about: files this proposal itself introduces, and endpoints marked
"(proposed — not implemented)". Anything else that is not a real 200 fails the check.

Usage: python3 scripts/check_links.py [--cache DIR] [--offline]      exit 1 on an undeclared broken link
"""
import argparse
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from factcheck import fetch, kind_of

ROOT = Path(__file__).resolve().parent.parent
FILES = ["proposed-llms.txt", "proposed-llms-install.md", "install-prompt-library.md"]
OUT = ROOT / "proposals/data/link-check.json"
# Introduced by the proposal itself, or explicitly marked as proposed. Everything else must resolve.
DECLARED_MISSING = {
    "https://xerj.org/llms-install.md": "introduced by this proposal (ship checklist step 1)",
    "https://xerj.org/api/field-report": "marked (proposed — not implemented) where it is printed",
}
SKIP = re.compile(r"^https?://(localhost|127\.0\.0\.1)|example\.|<|\$\{?")
URL_RE = re.compile(r"https?://[^\s)\"'`>\]]+")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cache", default=str(Path.home() / ".cache/xerj-llms-factcheck"))
    ap.add_argument("--offline", action="store_true")
    a = ap.parse_args()
    cache = Path(a.cache)
    cache.mkdir(parents=True, exist_ok=True)
    found = {}
    for name in FILES:
        for m in URL_RE.finditer((ROOT / "proposals" / name).read_text()):
            url = m.group(0).rstrip(".,;:·")
            url = url.split("#")[0]
            if SKIP.search(url) or "github.com/xerj-org/xerj/releases/download/" in url or url.endswith("/releases/download/$tag"):
                continue
            found.setdefault(url, set()).add(name)
    rows, bad = [], 0
    for url in sorted(found):
        meta = fetch(url, cache, a.offline)
        body = meta.pop("_body")
        text = body.read_bytes().decode("utf-8", "replace") if body.exists() else ""
        kind = kind_of(meta, text) if meta.get("status") else "error"
        ok = meta.get("status") == 200 and kind != "soft-404"
        declared = DECLARED_MISSING.get(url)
        verdict = "ok" if ok else (f"missing — declared: {declared}" if declared else "BROKEN")
        bad += verdict == "BROKEN"
        rows.append({"url": url, "in": sorted(found[url]), "status": meta.get("status"), "kind": kind,
                     "bytes": meta.get("bytes"), "fetched_at": meta.get("fetched_at"), "verdict": verdict})
    OUT.write_text(json.dumps(rows, indent=1) + "\n")
    print(f"{len(rows)} URLs, {sum(1 for r in rows if r['verdict'] == 'ok')} ok, "
          f"{sum(1 for r in rows if r['verdict'].startswith('missing'))} declared missing, {bad} BROKEN")
    for r in rows:
        if r["verdict"] != "ok":
            print("  ", r["verdict"], r["status"], r["url"])
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
