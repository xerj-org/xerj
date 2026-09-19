#!/usr/bin/env python3
"""Render one per-project page from one dissection record.

The layout is the one the 2026-09-18 pages use; `python3 scripts/build_pages.py --verify`
re-renders every 2026-09-18 record and reports how many committed pages it reproduces
byte for byte, so the ten pages added on 2026-09-19 are known to have the same shape.
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def fence(text: str, lang: str = "text") -> str:
    text = text.rstrip().lstrip("\n")
    ticks = "```"
    while ticks in text:
        ticks += "`"
    return f"{ticks}{lang}\n{text}\n{ticks}"


def block(title: str, text: str) -> list:
    return [f"### {title}", "", fence(text) if text else "_none_", ""]


def page(r: dict) -> str:
    l = r["llms_txt"]
    f = r.get("llms_full_txt") or {}
    date = r.get("dissected", "2026-09-18")
    how = r.get("how", "by one agent pass fetching the live URLs below. Quotes are verbatim as returned; "
                       "see ../README.md for what was and was not independently re-checked.")
    out = [f"# {r['project']}", "", f"Category: {r['category']}", "", f"Dissected {date} {how}", ""]
    out += ["## llms.txt", "",
            f"- URL: <{l['url']}> — HTTP {l['status']}",
            f"- Size: {l['bytes']} bytes, {l['lines']} lines",
            f"- Install position: {l['install_position']}",
            f"- Tone: {l['tone']}",
            f"- Section order: {' → '.join(l['section_order'])}", ""]
    if l.get("install_commands"):
        out += ["### Install commands found", "", fence("\n".join(l["install_commands"]), "sh"), ""]
    out += block("Text written for an agent (verbatim)", l.get("agent_prompt_verbatim"))
    out += block("MCP snippet (verbatim)", l.get("mcp_snippet_verbatim"))
    out += block("Feedback / contribution ask (verbatim)", l.get("feedback_cta_verbatim"))
    out += block("Best passage for driving install and first use (verbatim)", l.get("best_snippet_verbatim"))
    if l.get("one_click_links"):
        out += ["### One-click links", ""] + [f"- `{x}`" for x in l["one_click_links"]] + [""]
    out += ["## llms-full.txt", "",
            f"- URL: <{f.get('url')}> — HTTP {f.get('status')}",
            f"- Size: {f.get('bytes')} bytes",
            f"- Relationship to llms.txt: {f.get('relationship_to_llms_txt')}",
            f"- Install position: {f.get('install_position')}", ""]
    out += ["## Other agent-facing files", ""]
    for o in r.get("other_files") or []:
        out += [f"### {o['name']} — HTTP {o['status']}", "", f"<{o['url']}>", "", o.get("purpose") or "", ""]
        if o.get("key_verbatim"):
            out += [fence(o["key_verbatim"]), ""]
    for title, key in (("What they do better than XERJ", "what_they_do_better_than_xerj"),
                       ("What XERJ does better", "what_xerj_does_better"),
                       ("Adoptable ideas", "adoptable_ideas")):
        out += [f"## {title}", ""] + [f"- {x}" for x in r.get(key) or []] + [""]
    return "\n".join(out).rstrip("\n") + "\n"


def verify() -> int:
    from build_tables import load, slug

    recs, _, _ = load()
    same = diff = 0
    for r in recs:
        if r.get("page") == "generate":
            continue
        p = ROOT / "per-project" / f"{slug(r['project'])}.md"
        if p.exists() and p.read_text() == page(r):
            same += 1
        else:
            diff += 1
            print(f"differs: {p.name}")
    print(f"{same} of {same + diff} committed pages reproduced byte for byte")
    return 0 if diff == 0 else 1


if __name__ == "__main__":
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    if "--verify" in sys.argv:
        sys.exit(verify())
    print(__doc__)
