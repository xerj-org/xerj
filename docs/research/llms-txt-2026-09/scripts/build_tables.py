#!/usr/bin/env python3
"""Regenerate comparison-table.md (and any missing per-project page) from the data files.

Inputs   data/dissections.json            the 2026-09-18 pass (48 runs, 46 unique projects)
         data/dissections-2026-09-19.json the ten projects added on 2026-09-19 (optional)
Outputs  comparison-table.md
         per-project/<slug>.md            only for records that carry "page": "generate"

Nothing in the table is typed by hand. Every cell is read from a record:

  Install      "absent" when the record lists no install command, or when its
               install_position text begins with "absent"; otherwise the first
               44 characters of install_position.
  Agent text   "yes" when agent_prompt_verbatim is non-empty.
  One-click    len(one_click_links).
  MCP snippet  "yes" when mcp_snippet_verbatim is non-empty.
  Feedback ask "yes" when feedback_cta_verbatim is non-empty.

These are properties of what a dissecting pass *recorded*, not of the file itself.
The byte-level measurements of the files are in proposals/data/ and are produced
by scripts/measure_llms_txt.py.

Usage:  python3 scripts/build_tables.py            rewrite the outputs
        python3 scripts/build_tables.py --check    exit 1 if an output would change
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
XERJ = {"bytes": 40064, "lines": 234, "full": 83712}


def display(name: str) -> str:
    return re.split(r" \(| — ", name)[0].strip()


def slug(name: str) -> str:
    return re.sub(r"[^a-z0-9]+", "-", display(name).lower()).strip("-")


def n(v) -> str:
    return f"{int(v or 0):,}"


def load():
    runs = json.loads((ROOT / "data/dissections.json").read_text())
    extra_path = ROOT / "data/dissections-2026-09-19.json"
    extra = json.loads(extra_path.read_text()) if extra_path.exists() else []
    seen = {}
    for rec in runs + extra:
        # Weaviate and Deno were dissected twice on 2026-09-18. The fuller record
        # (longer JSON) is the one the per-project page was written from, so it wins.
        old = seen.get(rec["project"])
        if old is None or len(json.dumps(rec)) > len(json.dumps(old)):
            seen[rec["project"]] = rec
    return list(seen.values()), len(runs), len(extra)


def full_bytes(rec) -> int:
    """llms-full.txt size, counted only when the URL answered 200 (a 404 body is not the file)."""
    f = rec.get("llms_full_txt") or {}
    return int(f.get("bytes") or 0) if f.get("status") == 200 else 0


def install_cell(l) -> str:
    pos = (l.get("install_position") or "").strip()
    if not l.get("install_commands") or pos.lower().startswith("absent"):
        return "absent"
    return pos[:44].replace("|", "/")


def quantile(sorted_vals, q):
    return sorted_vals[int(len(sorted_vals) * q)]


def median(sorted_vals):
    k = len(sorted_vals)
    return sorted_vals[k // 2] if k % 2 else (sorted_vals[k // 2 - 1] + sorted_vals[k // 2]) // 2


def published(rec) -> bool:
    l = rec["llms_txt"]
    return l.get("status") == 200 and bool(l.get("bytes"))


def table(recs, n_first, n_extra) -> str:
    recs = sorted(recs, key=lambda r: (not published(r), r["llms_txt"].get("bytes") or 0, r["project"]))
    pub = [r for r in recs if published(r)]
    none = [r for r in recs if not published(r)]
    sizes = [r["llms_txt"]["bytes"] for r in pub]
    k, kp = len(recs), len(pub)
    rank = sum(1 for s in sizes if s < XERJ["bytes"]) + 1
    absent = sum(1 for r in pub if install_cell(r["llms_txt"]) == "absent")
    agent = sum(1 for r in pub if r["llms_txt"].get("agent_prompt_verbatim"))
    oneclick = sum(1 for r in pub if r["llms_txt"].get("one_click_links"))
    mcp = sum(1 for r in pub if r["llms_txt"].get("mcp_snippet_verbatim"))
    fb = sum(1 for r in pub if r["llms_txt"].get("feedback_cta_verbatim"))
    full = sum(1 for r in pub if full_bytes(r))
    if n_extra:
        title = (f"# Comparison table — {k} projects, {kp} of which publish an llms.txt "
                 f"({k - n_extra} measured 2026-09-18, {n_extra} added 2026-09-19)")
        src = ("Generated mechanically from `data/dissections.json` and `data/dissections-2026-09-19.json` by "
               "`scripts/build_tables.py` (`--check` fails if this file is stale)")
    else:
        title = f"# Comparison table — {k} projects' llms.txt, measured 2026-09-18"
        src = "Generated mechanically from `data/dissections.json`"
    out = [
        title,
        "",
        src + '. Sorted by llms.txt size. "Install" = whether installation commands appear in llms.txt itself.',
        "",
        f"**Distribution of llms.txt size (bytes):** min {n(sizes[0])} · p25 {n(quantile(sizes, .25))} · "
        f"median {n(median(sizes))} · p75 {n(quantile(sizes, .75))} · max {n(sizes[-1])}. "
        f"**XERJ: {n(XERJ['bytes'])} bytes, {XERJ['lines']} lines — rank {rank} of {kp + 1}.**",
        "",
        f"**Counts:** install commands absent from llms.txt in {absent}/{kp} · agent-addressed text in {agent} · "
        f"one-click links in {oneclick} · MCP snippet in {mcp} · a feedback/contribution ask in {fb} · "
        f"llms-full.txt published by {full}.",
        "",
    ]
    if none:
        out += [
            "The columns record what a dissecting pass wrote down for each project, so \"MCP snippet: yes\" can mean a snippet "
            "quoted from a linked page. What is literally inside each llms.txt — code fences, install commands, `claude mcp add` — "
            "is measured by script over a larger set in [`proposals/llms-txt-measurements.md`](proposals/llms-txt-measurements.md).",
            "",
            f"{len(none)} of the {n_extra} projects added on 2026-09-19 publish **no llms.txt at all**. They are XERJ's nearest "
            "functional peers (local code-search and MCP-server projects); their README is the agent-install surface. "
            "They are listed last and are excluded from the distribution and the counts above.",
            "",
        ]
    out += [
        "| Project | llms.txt bytes | lines | llms-full bytes | Install in llms.txt | Agent text | One-click | MCP snippet | Feedback ask |",
        "|---|---:|---:|---:|---|---|---:|---|---|",
    ]
    for r in pub:
        l = r["llms_txt"]
        out.append(
            f"| [{display(r['project'])}](per-project/{slug(r['project'])}.md) | {n(l.get('bytes'))} | {n(l.get('lines'))} | "
            f"{n(full_bytes(r))} | {install_cell(l)} | "
            f"{'yes' if l.get('agent_prompt_verbatim') else '—'} | {len(l.get('one_click_links') or [])} | "
            f"{'yes' if l.get('mcp_snippet_verbatim') else '—'} | {'yes' if l.get('feedback_cta_verbatim') else '—'} |"
        )
    out.append(
        f"| **XERJ (for reference)** | **{n(XERJ['bytes'])}** | **{XERJ['lines']}** | **{n(XERJ['full'])}** | "
        "line 11, inside 'Start here' | yes | 0 | — | yes |"
    )
    for r in none:
        l = r["llms_txt"]
        out.append(
            f"| [{display(r['project'])}](per-project/{slug(r['project'])}.md) | none published | — | — | "
            f"README only ({len(l.get('install_commands') or [])} install lines recorded) | — | "
            f"{len(l.get('one_click_links') or [])} | {'README' if l.get('mcp_snippet_verbatim') else '—'} | — |"
        )
    return "\n".join(out) + "\n"


def main():
    check = "--check" in sys.argv
    recs, n_first, n_extra = load()
    outputs = {ROOT / "comparison-table.md": table(recs, n_first, n_extra)}
    try:
        from build_pages import page  # optional: per-project pages for new records
        for r in recs:
            if r.get("page") == "generate":
                outputs[ROOT / "per-project" / f"{slug(r['project'])}.md"] = page(r)
    except ImportError:
        pass
    changed = [p for p, text in outputs.items() if not p.exists() or p.read_text() != text]
    if check:
        for p in changed:
            print(f"STALE {p.relative_to(ROOT)}")
        sys.exit(1 if changed else 0)
    for p in changed:
        p.write_text(outputs[p])
        print(f"wrote {p.relative_to(ROOT)}")
    if not changed:
        print("up to date")


if __name__ == "__main__":
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    main()
