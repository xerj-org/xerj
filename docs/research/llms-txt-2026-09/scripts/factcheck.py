#!/usr/bin/env python3
"""Re-fetch every claim in proposals/data/claims.json and record the literal result.

A claim is {id, source, claim, url, expect, quotes: [...]}.

  expect "present"  every quote must occur in the fetched body
  expect "absent"   no quote may occur in the fetched body
  expect "status"   the HTTP status must equal claim["status"]   (used for the 404 list)

Matching is fixed-string, after collapsing every run of whitespace to one space in
both the body and the quote. Nothing else is normalised for a "found". When that
fails, a second, looser comparison is tried and reported separately as
"found (loose)": case-folded, curly quotes and dashes straightened, HTML entities
unescaped, Markdown emphasis and backticks removed. A loose match is not a verbatim
match and is never reported as one.

Soft 404s: GitBook, Fern and Mintlify answer HTTP 200 with a "Page Not Found" body.
A 200 whose body is an HTML document when a text file was asked for, or whose first
2,000 characters contain a not-found marker, is recorded as kind "soft-404" and the
claim is reported as "not found" whatever the status code says.

Bodies are cached under --cache (default: a directory outside the repository) and are
never committed; the results carry URL, status, bytes, SHA-256 and fetch time.

Usage: python3 scripts/factcheck.py [--cache DIR] [--only ID[,ID...]] [--offline]
"""
import argparse
import datetime
import hashlib
import html
import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CLAIMS = ROOT / "proposals/data/claims.json"
RESULTS = ROOT / "proposals/data/factcheck-results.json"
REPORT = ROOT / "proposals/FACTCHECK.md"
UA = "Mozilla/5.0 (compatible; xerj-llms-factcheck/1.0; +https://xerj.org)"
NOT_FOUND_MARKERS = ("page not found", "404 not found", "404: not found", "404 - not found",
                     "this page could not be found", "page doesn't exist", "page does not exist",
                     "<title>404", "# 404", "not found | ")


# Code hosts render a file whose NAME ends in .md as an HTML page; that is the file, not a not-found page.
HTML_VIEWER = re.compile(r"^https://(github\.com/[^/]+/[^/]+/(blob|tree)/|codeberg\.org/[^/]+/[^/]+/?$)")


def fetch(url: str, cache: Path, offline: bool) -> dict:
    key = hashlib.sha1(url.encode()).hexdigest()[:16]
    body_path, meta_path = cache / f"{key}.body", cache / f"{key}.json"
    if meta_path.exists() and (offline or body_path.exists()):
        return json.loads(meta_path.read_text()) | {"_body": body_path}
    if offline:
        return {"url": url, "status": 0, "error": "offline and not cached", "_body": body_path}
    proc = subprocess.run(
        ["curl", "-sL", "--max-time", "60", "--compressed", "-A", UA, "-o", str(body_path),
         "-w", "%{http_code}\t%{content_type}\t%{url_effective}\t%{size_download}", url],
        capture_output=True, text=True)
    parts = (proc.stdout or "0\t\t\t0").split("\t")
    raw = body_path.read_bytes() if body_path.exists() else b""
    meta = {
        "url": url,
        "status": int(parts[0] or 0),
        "content_type": parts[1] if len(parts) > 1 else "",
        "final_url": parts[2] if len(parts) > 2 else "",
        "bytes": len(raw),
        "lines": raw.count(b"\n"),
        "sha256": hashlib.sha256(raw).hexdigest(),
        "fetched_at": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "curl_exit": proc.returncode,
    }
    meta_path.write_text(json.dumps(meta))
    return meta | {"_body": body_path}


def kind_of(meta: dict, text: str) -> str:
    if meta["status"] != 200:
        return "error"
    head = text[:2000].lower()
    wants_text = re.search(r"\.(md|mdx|txt|json|tsv)($|\?)", meta["url"]) is not None
    is_html = head.lstrip().startswith(("<!doctype", "<html"))
    if any(m in head for m in NOT_FOUND_MARKERS) and len(text) < 400_000 and (is_html or len(text) < 6000):
        return "soft-404"
    if wants_text and is_html and not HTML_VIEWER.search(meta["url"]):
        return "soft-404"
    return "html" if is_html else "text"


def json_strings(text: str) -> str:
    """Every string value of a JSON body, decoded, one per line (so \\u2013 matches an en dash)."""
    try:
        doc = json.loads(text)
    except ValueError:
        return ""
    out, stack = [], [doc]
    while stack:
        v = stack.pop()
        if isinstance(v, str):
            out.append(v)
        elif isinstance(v, dict):
            stack.extend(v.values())
        elif isinstance(v, list):
            stack.extend(v)
    return "\n".join(out)


def squash(s: str) -> str:
    return re.sub(r"\s+", " ", s).strip()


def loose(s: str) -> str:
    s = html.unescape(s)
    s = re.sub(r"<[^>]+>", " ", s)
    for a, b in (("“", '"'), ("”", '"'), ("‘", "'"), ("’", "'"), ("—", "-"), ("–", "-"), ("\u00a0", " ")):
        s = s.replace(a, b)
    s = re.sub(r"[*_`\\]", "", s)
    return squash(s).casefold()


def check(claim: dict, cache: Path, offline: bool) -> dict:
    meta = fetch(claim["url"], cache, offline)
    body_path = meta.pop("_body")
    text = body_path.read_bytes().decode("utf-8", "replace") if body_path.exists() else ""
    kind = kind_of(meta, text) if meta.get("status") else "error"
    if "json" in (meta.get("content_type") or "") and kind == "text":
        text = text + "\n" + json_strings(text)  # quotes are matched against decoded string values too
    res = {"id": claim["id"], "source": claim.get("source", ""), "claim": claim["claim"],
           "expect": claim.get("expect", "present"), **meta, "kind": kind, "quotes": []}
    if res["expect"] == "status":
        want = claim["status"]
        ok = (meta.get("status") == want) or (want == 404 and kind == "soft-404")
        res["verdict"] = "confirmed" if ok else "NOT CONFIRMED"
        res["detail"] = f"expected HTTP {want}, got {meta.get('status')} ({kind})"
        return res
    sq, lo = squash(text), None
    for q in claim.get("quotes", []):
        if kind in ("error", "soft-404"):
            hit = "not found"
        elif squash(q) in sq:
            hit = "found"
        else:
            lo = lo if lo is not None else loose(text)
            hit = "found (loose)" if loose(q) in lo else "not found"
        line = None
        if hit == "found":
            first = squash(q)[:40]
            for i, ln in enumerate(text.split("\n"), 1):
                if first[:25] in squash(ln):
                    line = i
                    break
        res["quotes"].append({"quote": q, "result": hit, "line": line})
    hits = [x["result"] for x in res["quotes"]]
    if res["expect"] == "absent":
        res["verdict"] = "confirmed" if all(h == "not found" for h in hits) and kind not in ("error", "soft-404") else "NOT CONFIRMED"
    elif all(h == "found" for h in hits):
        res["verdict"] = "confirmed"
    elif all(h.startswith("found") for h in hits):
        res["verdict"] = "confirmed (loose match — wording differs in markup only)"
    else:
        res["verdict"] = "NOT CONFIRMED"
    return res


def cell(s: str) -> str:
    return s.replace("|", "\\|").replace("\n", " ")


def report(results: list) -> str:
    today = sorted(r.get("fetched_at", "") for r in results if r.get("fetched_at"))
    span = f"{today[0]} – {today[-1]}" if today else "n/a"
    groups = {}
    for r in results:
        groups.setdefault(r["source"], []).append(r)
    ok = sum(1 for r in results if r["verdict"].startswith("confirmed"))
    out = ["# Fact-check — literal results", "",
           "Produced by `scripts/factcheck.py` from `proposals/data/claims.json`. Do not edit by hand; "
           "re-run the script. The machine-readable copy, with SHA-256 of every body, is "
           "`proposals/data/factcheck-results.json`.", "",
           f"- Fetched: {span} (UTC), with `curl -sL --compressed` and the User-Agent `{UA}`.",
           f"- Claims checked: **{len(results)}** · confirmed: **{ok}** · not confirmed: **{len(results) - ok}**.",
           "- \"found\" = the quote occurs in the body as a fixed string after collapsing whitespace. "
           "\"found (loose)\" = it occurs only after case-folding and removing Markdown/HTML markup and "
           "typographic quotes; that is not a verbatim match and the report does not present it as one.",
           "- A 200 whose body is a not-found page is recorded as `soft-404` and counts as not found.",
           "- A claim that was not confirmed is either corrected in `report.md` or removed from it; "
           "the \"Disposition\" section at the end says which.", ""]
    titles = {"queued-2026-09-18": "A. The 25 claims queued by the synthesis pass on 2026-09-18",
              "critic-2026-09-18": "B. Claims and URLs supplied by the completeness critic",
              "new-2026-09-19": "C. Claims added on 2026-09-19 (new dissections, client coverage, conventions)",
              "xerj-site": "D. XERJ's own site — what exists and what returns 404"}
    for src in list(titles) + [s for s in groups if s not in titles]:
        rs = groups.get(src)
        if not rs:
            continue
        out += [f"## {titles.get(src, src)}", "",
                "| # | URL | What was looked for | Result | HTTP · bytes · kind | Fetched (UTC) |",
                "|---|---|---|---|---|---|"]
        for r in rs:
            if r["expect"] == "status":
                looked = r["detail"]
            else:
                looked = "<br>".join(
                    ("must be ABSENT: " if r["expect"] == "absent" else "")
                    + "“" + cell(q["quote"][:220]) + ("…" if len(q["quote"]) > 220 else "") + "” → **"
                    + q["result"] + "**" + (f" (line {q['line']})" if q.get("line") else "")
                    for q in r["quotes"])
            out.append(f"| {r['id']} | {r['url']} | {cell(r['claim'])}<br>{looked} | **{r['verdict']}** | "
                       f"{r.get('status')} · {r.get('bytes', 0):,} · {r['kind']} | {r.get('fetched_at', '')} |")
        out.append("")
    return "\n".join(out)


def links_section() -> str:
    path = ROOT / "proposals/data/link-check.json"
    if not path.exists():
        return ""
    rows = json.loads(path.read_text())
    out = ["## E. Every URL printed in the three proposal files", "",
           "Produced by `scripts/check_links.py`. A proposal may link only to pages that exist; the two exceptions are declared "
           "in that script and are what `report.md` §9 (the ship checklist) is about.", "",
           "| URL | Printed in | Result | HTTP · bytes · kind | Fetched (UTC) |", "|---|---|---|---|---|"]
    for r in rows:
        out.append(f"| {r['url']} | {', '.join(x.replace('proposed-', '').replace('.md', '').replace('.txt', '') for x in r['in'])} | "
                   f"**{r['verdict']}** | {r['status']} · {r['bytes'] or 0:,} · {r['kind']} | {r['fetched_at'] or ''} |")
    return "\n".join(out) + "\n\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cache", default=str(Path.home() / ".cache/xerj-llms-factcheck"))
    ap.add_argument("--only", default="")
    ap.add_argument("--offline", action="store_true")
    a = ap.parse_args()
    cache = Path(a.cache)
    cache.mkdir(parents=True, exist_ok=True)
    claims = json.loads(CLAIMS.read_text())
    only = set(filter(None, a.only.split(",")))
    results = [check(c, cache, a.offline) for c in claims if not only or c["id"] in only]
    if only:
        print(json.dumps(results, indent=1, ensure_ascii=False))
        return
    RESULTS.write_text(json.dumps(results, indent=1, ensure_ascii=False) + "\n")
    disposition = ROOT / "proposals/data/factcheck-disposition.md"
    REPORT.write_text(report(results) + links_section() + (disposition.read_text() if disposition.exists() else ""))
    bad = [r for r in results if not r["verdict"].startswith("confirmed")]
    print(f"{len(results)} claims, {len(results) - len(bad)} confirmed, {len(bad)} not confirmed")
    for r in bad:
        print("  NOT CONFIRMED", r["id"], r["url"], r.get("status"), r["kind"])


if __name__ == "__main__":
    main()
