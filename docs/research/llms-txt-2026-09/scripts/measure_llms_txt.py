#!/usr/bin/env python3
"""Fetch every llms.txt in proposals/data/llms-txt-urls.tsv, measure it, and write the manifest.

Outputs  proposals/data/llms-txt-manifest.json   one row per URL: status, kind, bytes, lines, SHA-256,
                                                  fetch time and the measures below. No bodies.
         proposals/llms-txt-measurements.md      the distribution table the report quotes.

Third-party bodies are cached outside the repository and never committed; the manifest holds
hashes and URLs so a reader can tell whether a file has changed since it was measured.

Definitions (each is a choice; change one and the numbers change):

  kept            HTTP 200, not HTML, not a not-found page, and not byte-identical (SHA-256) to a
                  file already kept. Everything else is listed in the manifest with kind != "text".
  line            split on "\n".
  fenced          inside a ``` or ~~~ block.
  prose line      not fenced, not blank, and not a heading (#), list item (-, *, +, "1."), table
                  row (|), or HTML tag line (<).  Blockquotes count as prose.
  prose bytes     UTF-8 bytes of prose lines.
  long line       ANY unfenced line over 400 characters (list items included: a 1,000-character
                  numbered step is as hard to scan as a 1,000-character paragraph).
  long prose line a prose line over 400 characters (the stricter reading).
  obligation      case-insensitive whole-word matches of: required, must, owe/owes/owed, obligat*,
                  never, always, "do not", "don't" — anywhere in the file.
  install command first match, anywhere, of a pipe-to-shell installer or a package-manager install
                  (see INSTALL_RE). "bytes before install" is its byte offset.
  percentile      share of kept files whose value is <= the subject's value.

Usage: python3 scripts/measure_llms_txt.py [--cache DIR] [--offline]
"""
import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from factcheck import fetch, kind_of  # same fetcher, same soft-404 rule

ROOT = Path(__file__).resolve().parent.parent
URLS = ROOT / "proposals/data/llms-txt-urls.tsv"
MANIFEST = ROOT / "proposals/data/llms-txt-manifest.json"
REPORT = ROOT / "proposals/llms-txt-measurements.md"
PROPOSAL = ROOT / "proposals/proposed-llms.txt"

OBLIGATION_RE = re.compile(r"\b(required|must|owe[sd]?|obligat\w*|never|always|do not|don't|don’t)\b", re.I)
INSTALL_RE = re.compile(
    r"(curl[^\n|]*\|\s*(?:sudo\s+)?(?:ba)?sh\b|irm[^\n|]*\|\s*iex\b|iwr[^\n|]*\|\s*iex\b|"
    r"\bnpm (?:i|install|create)\b|\bpnpm (?:add|create)\b|\byarn add\b|\bbun add\b|\bpip3? install\b|"
    r"\buv (?:add|tool install|pip install)\b|\bbrew install\b|\bcargo install\b|\bgo install\b|"
    r"\bdocker (?:run|pull)\b|\bnpx (?:-y |--yes )?[@\w])", re.I)
STRUCT_RE = re.compile(r"^\s*(#|[-*+]\s|\d+[.)]\s|\||<)")


def optional_demands(text: str):
    """required / must / owe* / obligat* INSIDE the `## Optional` section; None when there is no such section."""
    m = re.search(r"^## Optional\s*$(.*?)(?=^## |\Z)", text, flags=re.M | re.S)
    return None if not m else len(re.findall(r"\b(required|must|owe[sd]?|obligat\w*)\b", m.group(1), flags=re.I))


def measure(text: str) -> dict:
    fenced = False
    prose_bytes = long_lines = long_prose = fences = 0
    longest = 0
    for ln in text.split("\n"):
        if re.match(r"^\s*(```|~~~)", ln):
            fenced = not fenced
            fences += 1
            continue
        if fenced:
            continue
        longest = max(longest, len(ln))
        if len(ln) > 400:
            long_lines += 1
        if ln.strip() and not STRUCT_RE.match(ln):
            prose_bytes += len(ln.encode())
            if len(ln) > 400:
                long_prose += 1
    m = INSTALL_RE.search(text)
    return {
        "bytes": len(text.encode()),
        "lines": text.count("\n"),
        "prose_bytes": prose_bytes,
        "long_lines_400": long_lines,
        "long_prose_lines_400": long_prose,
        "longest_unfenced_line": longest,
        "obligation_words": len(OBLIGATION_RE.findall(text)),
        "code_fences": fences // 2,
        "install_command": m.group(0)[:60] if m else None,
        "bytes_before_install": len(text[:m.start()].encode()) if m else None,
        "claude_mcp_add": len(re.findall(r"claude mcp add", text)),
        "mcpServers": text.count("mcpServers"),
        "npx_skills_add": len(re.findall(r"npx (?:-y )?skills add", text)),
        "has_optional_h2": bool(re.search(r"^## Optional\s*$", text, re.M)),
        "optional_section_demand_words": optional_demands(text),
    }


def quantiles(vals):
    s = sorted(vals)
    k = len(s)
    q = lambda p: s[min(k - 1, int(k * p))]
    med = s[k // 2] if k % 2 else (s[k // 2 - 1] + s[k // 2]) / 2
    return {"min": s[0], "p25": q(.25), "median": med, "p75": q(.75), "p90": q(.90), "max": s[-1]}


def pct(vals, v):
    return round(100 * sum(1 for x in vals if x <= v) / len(vals))


def ordinal(k: int) -> str:
    suffix = "th" if 10 <= k % 100 <= 20 else {1: "st", 2: "nd", 3: "rd"}.get(k % 10, "th")
    return f"{k}{suffix}"


def fmt(v):
    return "—" if v is None else (f"{v:,.0f}" if float(v).is_integer() else f"{v:,.1f}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--cache", default=str(Path.home() / ".cache/xerj-llms-factcheck"))
    ap.add_argument("--offline", action="store_true")
    a = ap.parse_args()
    cache = Path(a.cache)
    cache.mkdir(parents=True, exist_ok=True)

    rows, seen_hash = [], {}
    for line in URLS.read_text().splitlines():
        if not line or line.startswith("#"):
            continue
        name, url, origin = line.split("\t")
        meta = fetch(url, cache, a.offline)
        body = meta.pop("_body")
        text = body.read_bytes().decode("utf-8", "replace") if body.exists() else ""
        kind = kind_of(meta, text) if meta.get("status") else "error"
        row = {"name": name, "origin": origin, **meta, "kind": kind}
        if kind == "text":
            if meta["sha256"] in seen_hash and origin != "subject":
                row["kind"] = f"duplicate of {seen_hash[meta['sha256']]}"
            else:
                seen_hash[meta["sha256"]] = url
                row["measures"] = measure(text)
        rows.append(row)

    subject = next(r for r in rows if r["origin"] == "subject")
    peers = [r for r in rows if r.get("measures") and r["origin"] != "subject"]
    allfiles = peers + [subject]
    proposal = measure(PROPOSAL.read_text()) if PROPOSAL.exists() else None
    MANIFEST.write_text(json.dumps({"rows": rows, "proposal": proposal}, indent=1, ensure_ascii=False) + "\n")

    dropped = [r for r in rows if not r.get("measures")]
    when = sorted(r["fetched_at"] for r in rows if r.get("fetched_at"))
    keys = [("bytes", "Bytes"), ("lines", "Lines"), ("prose_bytes", "Prose bytes"),
            ("long_lines_400", "Unfenced lines over 400 characters"),
            ("long_prose_lines_400", "Prose lines over 400 characters (stricter)"),
            ("longest_unfenced_line", "Longest unfenced line (characters)"),
            ("obligation_words", "Obligation words")]
    out = ["# llms.txt measurements", "",
           "Produced by `scripts/measure_llms_txt.py`; do not edit by hand. Definitions are in the script's "
           "docstring. Per-file rows (URL, status, bytes, SHA-256, fetch time, every measure) are in "
           "`data/llms-txt-manifest.json`. Third-party bodies are not committed.", "",
           f"- URLs fetched: **{len(rows)}** ({when[0]} – {when[-1]} UTC). Kept: **{len(allfiles)}** "
           f"({len(peers)} peers + XERJ). Dropped: **{len(dropped)}** "
           f"({', '.join(sorted(set(r['kind'].split(' of ')[0] for r in dropped)))}).",
           "- Generated index files change daily; re-run before quoting a number.", "",
           "| Measure | min | p25 | median | p75 | p90 | max | XERJ today | XERJ percentile | Proposal | Proposal percentile |",
           "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|"]
    for k, label in keys:
        vals = [r["measures"][k] for r in allfiles]
        q = quantiles(vals)
        sv = subject["measures"][k]
        pv = proposal[k] if proposal else None
        out.append(f"| {label} | {fmt(q['min'])} | {fmt(q['p25'])} | {fmt(q['median'])} | {fmt(q['p75'])} | {fmt(q['p90'])} | "
                   f"{fmt(q['max'])} | {fmt(sv)} | {ordinal(pct(vals, sv))} | {fmt(pv)} | "
                   f"{ordinal(pct(vals, pv)) if pv is not None else '—'} |")
    n = len(peers)
    has_install = [r for r in peers if r["measures"]["install_command"]]
    has_fence = [r for r in peers if r["measures"]["code_fences"]]
    out += ["", "## Counts over the kept peer files", "",
            f"- An install-like command anywhere in llms.txt (regex `INSTALL_RE`): **{len(has_install)} of {n}**.",
            f"- At least one fenced code block: **{len(has_fence)} of {n}**.",
            f"- Neither a fenced block nor an install-like command: **{sum(1 for r in peers if not r['measures']['code_fences'] and not r['measures']['install_command'])} of {n}**.",
            f"- `claude mcp add` in llms.txt itself: **{sum(1 for r in peers if r['measures']['claude_mcp_add'])} of {n}**"
            f" ({', '.join(r['name'] for r in peers if r['measures']['claude_mcp_add']) or 'none'}).",
            f"- `mcpServers` in llms.txt itself: **{sum(1 for r in peers if r['measures']['mcpServers'])} of {n}**"
            f" ({', '.join(r['name'] for r in peers if r['measures']['mcpServers']) or 'none'}).",
            f"- `npx skills add` in llms.txt itself: **{sum(1 for r in peers if r['measures']['npx_skills_add'])} of {n}**.",
            f"- A `## Optional` heading: **{sum(1 for r in peers if r['measures']['has_optional_h2'])} of {n}**. "
            f"Of those, files with \"required\", \"must\", \"owe\" or \"obligat…\" inside that section: "
            f"**{sum(1 for r in peers if r['measures']['optional_section_demand_words'])}**. "
            f"XERJ's `## Optional` section contains **{subject['measures']['optional_section_demand_words']}**.",
            "", "## XERJ today and the proposal", "",
            "| | XERJ today | Proposal |", "|---|---:|---:|"]
    for k, label in keys + [("bytes_before_install", "Bytes before the first install command"), ("code_fences", "Fenced code blocks"),
                            ("claude_mcp_add", "`claude mcp add` lines"), ("mcpServers", "`mcpServers` occurrences")]:
        out.append(f"| {label} | {fmt(subject['measures'][k])} | {fmt(proposal[k]) if proposal else '—'} |")
    top = sorted((r for r in allfiles if r["measures"]["bytes"] < 100_000), key=lambda r: -r["measures"]["obligation_words"])[:8]
    out += ["", "## Highest obligation-word counts among files under 100 KB", "",
            "| File | Obligation words | Bytes |", "|---|---:|---:|"]
    out += [f"| {r['name']} — {r['url']} | {r['measures']['obligation_words']} | {r['measures']['bytes']:,} |" for r in top]
    out += ["", "## Size histogram (kept files)", "", "```text"]
    edges = [(0, 5_000), (5_000, 10_000), (10_000, 20_000), (20_000, 40_000), (40_000, 80_000), (80_000, 200_000), (200_000, 10**12)]
    sb = subject["measures"]["bytes"]
    for lo, hi in edges:
        c = sum(1 for r in allfiles if lo <= r["measures"]["bytes"] < hi)
        label = f"{lo // 1000:>4} – {hi // 1000:>3} KB" if hi < 10**12 else "      > 200 KB"
        out.append(f"{label}  {c:>3}  {'#' * c}{'   <- XERJ ' + format(sb, ',') if lo <= sb < hi else ''}")
    out += ["```", ""]
    REPORT.write_text("\n".join(out))
    print(f"{len(rows)} urls, {len(allfiles)} kept, {len(dropped)} dropped → {REPORT.relative_to(ROOT)}")


if __name__ == "__main__":
    main()
