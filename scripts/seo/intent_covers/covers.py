#!/usr/bin/env python3
"""Render the og:image cards for the agent-intent answer pages.

    landing/og/answers-<slug>.png   1200x630, 96-colour PNG

Same series as the blog covers (scripts/seo/blog_covers/, #1918): light paper
#f6f4ee, ink #0b0b0d, ochre #A06800 for the subtitle, yellow #FFC400 as a fill
only. Layout, CSS, fonts, grid background and the PNG quantiser are imported
from there. No grain, no gradient, no shadow. Only the kicker, the footer URL
and the motifs (intent_motifs.py) change. Every number on a card is a measured
value printed on its page.

The structured-data card is not rendered. That page is held: the feature is
still planned (issue #1797). The motif function stays in intent_motifs.py.

    python3 scripts/seo/intent_covers/covers.py
    python3 scripts/seo/intent_covers/covers.py --contact /tmp/answers.png

An offline authoring tool like blog_covers: CI does not run it and the PNGs
are committed. Needs playwright + pillow and a Chromium (or CHROME), plus
scripts/seo/blog_covers/ present in the tree.
"""

from __future__ import annotations

import argparse
import html
import importlib.util
import io
import pathlib
import random
import sys

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent.parent.parent
LANDING = REPO / "landing"
BLOG = HERE.parent / "blog_covers"

sys.path.insert(0, str(BLOG))
_spec = importlib.util.spec_from_file_location("blog_covers_covers", BLOG / "covers.py")
bc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bc)
sys.path.insert(0, str(HERE))
from intent_motifs import MOTIFS  # noqa: E402

DATE = "2026-10-19"

# title / subtitle: the page's own question and verdict; alt: what the card shows.
# Held, not rendered: extract-structured-data-from-web-page-json-schema (#1797).
CARDS: list[dict[str, str]] = [
    dict(slug="local-code-search-mcp-fewer-tokens-than-grep", motif="bytes",
         title="Local code search for agents, fewer tokens than grep",
         subtitle="Read the passage, not the 17 files.",
         alt="Cover: dot-bars of the bytes an agent reads for one question on psf/requests: rg 25,150 B, xerj search 1,303 B (sessions.py:309), xerj def 949 B (sessions.py:154)"),
    dict(slug="full-text-search-over-jsonl-files-cli", motif="rows",
         title="Full-text search over JSONL and CSV from the CLI",
         subtitle="BM25 over every row, no schema to write.",
         alt="Cover: a field of 1,000 dots for the CSV rows, 79 ink BM25 hits for 'bm25 jsonl folder' in 1 ms and row id 480 ringed in yellow; an exact rg phrase search found 0"),
    dict(slug="ask-questions-about-a-github-repo-without-cloning", motif="ask",
         title="Ask questions about a GitHub repo without cloning it",
         subtitle="One signed pack, a local node, a file:line answer.",
         alt="Cover: a question routed across the 14 corpora of the signed catalogue to the snappy tile, then to a local node on port 9300 with the answer at snappy.cc:1643; cold 2,356 ms, warm 30 to 40 ms"),
    dict(slug="batch-lookup-cve-ghsa-ids-offline", motif="batch",
         title="Batch CVE and GHSA lookup, offline",
         subtitle="Six IDs, one query, two honest abstains.",
         alt="Cover: six advisory IDs flow into one query over the signed rust-vulns pack of 2,033 advisories; four resolve to RUSTSEC or GHSA records, two abstain; 15 ms"),
    dict(slug="retrieval-that-says-not-found-with-file-line-citations", motif="abstain",
         title="Retrieval that cites file:line and says not found",
         subtitle="Exact lookups abstain. Free text does not, yet.",
         alt="Cover: three dot discs for three outcomes: answered at zstd.h:134 with exit 0, no match with exit 1, and an off-topic question that still returned passages with exit 0 (issue #1786)"),
]


def page(card: dict, number: int, total: int) -> str:
    L = bc.layout("og")
    rng = random.Random(card["slug"] + ":og")
    motif = MOTIFS[card["motif"]](L["mw"], L["mh"], rng)
    motif_svg = (f'<svg class="motif" width="{L["mw"]}" height="{L["mh"] + 4}" '
                 f'viewBox="0 0 {L["mw"]} {L["mh"] + 4}" overflow="visible" xmlns="http://www.w3.org/2000/svg">'
                 f'{motif}</svg>')
    css = bc.CSS % {**bc.TOKENS, **L, "display": bc.font_url("BigShouldersDisplay-wght.ttf"),
                    "mono": bc.font_url("JetBrainsMono-wght.ttf")}
    e = html.escape
    return f"""<!DOCTYPE html><html><head><meta charset="utf-8"><style>{css}
h1 {{ font-size: 64px; }}</style></head><body>
<div class="card">
{bc.background(L["W"], L["H"], 0)}
<div class="kicker"><span class="sq"></span><span>XERJ · ANSWERS FOR AGENTS · MEASURED {DATE}</span>
<span class="no">No. {number:02d} / {total:02d}</span></div>
<div class="textcol"><div class="rule"></div><h1>{e(card["title"])}</h1><p class="sub">{e(card["subtitle"])}</p></div>
{motif_svg}
<div class="foot"><span><b>xerj.org/answers/{e(card["slug"])}</b></span><span class="r">DRAWN FROM THE PAGE'S OWN RUN</span></div>
</div></body></html>"""


def render(cards: list[dict]) -> dict[str, object]:
    from playwright.sync_api import sync_playwright
    from PIL import Image
    out = {}
    with sync_playwright() as p:
        exe = bc.chrome_path()
        if not exe:
            raise SystemExit("no Chrome/Chromium found; set CHROME")
        browser = p.chromium.launch(executable_path=exe)
        for n, card in enumerate(cards, start=1):
            pg = browser.new_page(viewport={"width": 1200, "height": 630}, device_scale_factor=1)
            tmp = HERE / ".render-og.html"
            tmp.write_text(page(card, n, len(cards)))
            pg.goto(tmp.as_uri())
            pg.evaluate("document.fonts.ready")
            pg.wait_for_timeout(150)
            pg.evaluate(bc.FIT_JS)
            png = pg.locator(".card").screenshot(type="png")
            tmp.unlink(missing_ok=True)
            pg.close()
            out[card["slug"]] = Image.open(io.BytesIO(png)).convert("RGB")
        browser.close()
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--contact", type=pathlib.Path, help="also write a contact sheet PNG")
    a = ap.parse_args()
    images = render(CARDS)
    for slug, img in images.items():
        path = LANDING / "og" / f"answers-{slug}.png"
        bc.save_png(img, path)
        print(f"wrote {path.relative_to(REPO)} ({path.stat().st_size:,} B)")
    if a.contact:
        bc.contact_sheet({s: {"og": i} for s, i in images.items()}, a.contact)
        print(f"wrote {a.contact}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
