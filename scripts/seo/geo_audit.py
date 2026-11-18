#!/usr/bin/env python3
"""GEO (answer-engine) audit of the generated answers/ and compare/ pages.

For every article it reports the signals an AI answer engine uses to lift and
cite a page:

  * tldr_words     — words in the TL;DR paragraph (the older summary)
  * answer_words   — words in the answer-first "Short answer" span (40-60 wanted)
  * restates_q     — does that span reuse the question's content words?
  * key_fact       — a dated, sourced number near the top (answer card)
  * h2             — number of H2 sections (a clear outline)
  * faqpage/howto  — FAQPage / HowTo JSON-LD in the committed HTML.
                     Reported, not required: no page opts in until that is decided.
  * date_modified  — dateModified in the article JSON-LD
  * twin           — Markdown twin on disk, carrying the short answer when there is one
  * intent_group   — the agent-intent cluster used by llms.txt
  * queries        — literal agent queries the page targets
  * related        — internal links in the Related block

    python3 scripts/seo/geo_audit.py                 # table on stdout
    python3 scripts/seo/geo_audit.py --json          # machine-readable
    python3 scripts/seo/geo_audit.py --root DIR      # audit another checkout
    python3 scripts/seo/geo_audit.py --fail          # exit 1 if any page misses a signal

Standard library only, like the rest of scripts/seo/.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import re
import sys

STOP = set("a an and are can do does for from how i in is it my of on or the to what "
           "when which with without you your xerj me we our this that be at as by not "
           "no if so there any just should".split())
JSONLD_RE = re.compile(r'<script type="application/ld\+json">\s*(.*?)\s*</script>', re.S)
CARD_RE = re.compile(r'<div class="prompt-card answer-first" id="short-answer">(.*?)</div>', re.S)
TAG_RE = re.compile(r"<[^>]+>")


def words(text: str) -> list[str]:
    return re.findall(r"[a-z0-9]+", text.lower())


def content_words(text: str) -> set[str]:
    return {w for w in words(text) if w not in STOP and len(w) > 2}


def audit(root: pathlib.Path) -> list[dict]:
    sys.path.insert(0, str(root / "scripts" / "seo"))
    import article_data  # type: ignore

    rows = []
    for category in ("answers", "compare"):
        for src in sorted((root / "content" / category).glob("*.md")):
            art = article_data.load(src, category)
            html_path = root / "landing" / category / f"{art.slug}.html"
            twin_path = root / "landing" / category / f"{art.slug}.md"
            page = html_path.read_text(encoding="utf-8") if html_path.exists() else ""
            twin = twin_path.read_text(encoding="utf-8") if twin_path.exists() else ""
            types: list[str] = []
            modified = ""
            for raw in JSONLD_RE.findall(page):
                try:
                    node = json.loads(raw)
                except json.JSONDecodeError:
                    continue
                types.append(node.get("@type", ""))
                if node.get("@type") in ("TechArticle", "Article"):
                    modified = node.get("dateModified", "")
            answer = getattr(art, "answer", None) or ""
            card = CARD_RE.search(page)
            answer_words = len(answer.split()) if answer else 0
            qwords = content_words(art.question)
            overlap = len(qwords & content_words(answer)) / max(1, len(qwords)) if answer else 0.0
            related = page.split("<h2>Related</h2>", 1)[-1].split("</ul>", 1)[0] if page else ""
            first = art.body.strip().split("\n\n", 1)[0]
            tldr = re.sub(r"^\*\*TL;DR\*\*\s*[—–:-]?\s*", "", first.strip())
            rows.append({
                "page": f"{category}/{art.slug}",
                "tldr_words": len(tldr.split()),
                "title": art.title,
                "title_len": len(art.title),
                "description_len": len(art.description),
                "answer_words": answer_words,
                "restates_q": round(overlap, 2),
                "key_fact": bool(getattr(art, "key_fact", None)) and bool(card and "Key number" in card.group(1)),
                "key_fact_date": getattr(art, "key_fact_date", None) or "",
                "h2": len(re.findall(r"^## ", art.body, re.M)) + 1,  # + the generated FAQ H2
                "faqpage": "FAQPage" in types,
                "howto": "HowTo" in types,
                "date_modified": modified,
                "twin": bool(twin) and (not answer or "**Short answer**" in twin),
                "intent_group": getattr(art, "intent_group", None) or "",
                "queries": list(getattr(art, "queries", ()) or ()),
                "related": related.count("<li>"),
            })
    return rows


def missing(row: dict) -> list[str]:
    out = []
    if not 40 <= row["answer_words"] <= 60:
        out.append("answer-first")
    if not row["key_fact"]:
        out.append("key-fact")
    if not row["date_modified"]:
        out.append("dateModified")
    if not row["twin"]:
        out.append("twin")
    if not row["intent_group"]:
        out.append("intent")
    if row["related"] < 3:
        out.append("related<3")
    return out


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--root", default=str(pathlib.Path(__file__).resolve().parents[2]))
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--fail", action="store_true")
    args = ap.parse_args(argv)
    rows = audit(pathlib.Path(args.root))
    if args.json:
        print(json.dumps(rows, indent=1))
    else:
        print("| page | answer words | key fact | H2 | FAQPage | HowTo | modified | intent | queries | related | missing |")
        print("|---|---|---|---|---|---|---|---|---|---|---|")
        for r in rows:
            print(f"| {r['page']} | {r['answer_words']} | {'yes' if r['key_fact'] else 'no'} | "
                  f"{r['h2']} | {'yes' if r['faqpage'] else 'no'} | {'yes' if r['howto'] else 'no'} | "
                  f"{r['date_modified']} | {r['intent_group'] or '-'} | {len(r['queries'])} | "
                  f"{r['related']} | {', '.join(missing(r)) or '-'} |")
    bad = [r for r in rows if missing(r)]
    if args.fail and bad:
        print(f"geo_audit: {len(bad)} of {len(rows)} page(s) miss a GEO signal", file=sys.stderr)
        return 1
    print(f"geo_audit: {len(rows)} pages, {len(rows) - len(bad)} complete", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
