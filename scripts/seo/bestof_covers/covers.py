#!/usr/bin/env python3
"""Render the og:image cards for the "best of 2026" compare pages.

    landing/og/compare-<slug>.png   1200x630, 96-colour PNG

Same series as the blog covers (scripts/seo/blog_covers/, #1918) and the
agent-intent covers: light paper #f6f4ee, ink #0b0b0d, ochre #A06800 for the
subtitle, yellow #FFC400 as a fill on the XERJ row only. Layout, CSS, fonts,
grid background and the PNG quantiser come from blog_covers. No grain, no
gradient, no shadow. Every number a card draws is printed in the summary
table or benchmark of the page it illustrates (checked 2026-10-19).

    python3 scripts/seo/bestof_covers/covers.py [--contact /tmp/bestof.png]

Offline authoring tool: CI does not run it and the PNGs are committed. Needs
playwright + pillow and a Chromium (or CHROME), plus scripts/seo/blog_covers/.
"""

from __future__ import annotations

import argparse
import html
import importlib.util
import io
import os
import pathlib
import random
import sys

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent.parent.parent
LANDING = REPO / "landing"
BLOG = pathlib.Path(os.environ.get("BLOG_COVERS", HERE.parent / "blog_covers"))

sys.path.insert(0, str(BLOG))
_spec = importlib.util.spec_from_file_location("blog_covers_covers", BLOG / "covers.py")
bc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(bc)
from motifs import dotbar, hair, label  # noqa: E402

DATE = "2026-10-19"


def m_bars(rows: list[tuple[str, float, str]], unit_note: str):
    """Horizontal dot-bars to scale (linear, longest bar = full width).
    XERJ's row is yellow, the rest in the comparison ink."""
    def draw(w: float, h: float, rng: random.Random) -> str:
        out = []
        full = w * 0.80
        top = max(v for _, v, _ in rows)
        pitch = 6.0
        gap = (h - 34) / len(rows)
        y = 14.0
        for name, value, note in rows:
            xerj = name.startswith("XERJ")
            ink = "y" if xerj else "b"
            length = max(pitch, full * value / top)
            out.append(label(0, y, name, "lab w" if xerj else "lab"))
            out.append(dotbar(0, y + 7, length, 3, pitch, ink, 3))
            out.append(label(length + 10, y + 7 + 2.4 * pitch, note, "num y" if xerj else "num b"))
            y += gap
        out.append(hair(0, h - 16, full, h - 16, "f"))
        out.append(label(w, h - 2, unit_note, "tag m", "end"))
        return "".join(out)
    return draw


CARDS: list[dict] = [
    dict(slug="best-mcp-server-local-code-search-2026",
         title="The best MCP servers for local code search, 2026",
         subtitle="Seven servers, four jobs. Serena beats us on refactors.",
         motif=m_bars([("SERENA", 30404, "30,404"), ("CLAUDE CONTEXT", 12627, "12,627"),
                       ("XERJ", 10569, "10,569"), ("SEMBLE", 6248, "6,248"),
                       ("COCOINDEX-CODE", 2930, "2,930"), ("CODEDB", 1395, "1,395"),
                       ("AST-GREP MCP", 482, "482")], "GITHUB STARS · 2026-10-19")),
    dict(slug="best-grep-alternative-for-ai-coding-agents-2026",
         title="The best grep alternatives for AI coding agents, 2026",
         subtitle="ripgrep is still the default. Here is when it isn't.",
         motif=m_bars([("RIPGREP", 69257, "69,257"), ("THE SILVER SEARCHER", 27128, "27,128"),
                       ("SEMGREP", 17138, "17,138"), ("AST-GREP", 16288, "16,288"),
                       ("XERJ", 10569, "10,569"), ("UGREP", 3312, "3,312"),
                       ("ZOEKT", 1978, "1,978")], "GITHUB STARS · 2026-10-19")),
    dict(slug="best-local-full-text-search-jsonl-csv-2026",
         title="The best local full-text search for JSONL and CSV, 2026",
         subtitle="Benchmarked: SQLite FTS5 wins. XERJ ran out of memory at 300k.",
         motif=m_bars([("SQLITE FTS5 3.46.1", 0.29, "0.29 S"), ("DUCKDB FTS 1.5.6", 0.61, "0.61 S"),
                       ("XERJ RC.97 · 9.1 GIB PEAK", 70.57, "70.57 S")],
                      "INDEX TIME · 50,000 JSONL RECORDS · 2026-10-19")),
    dict(slug="best-lightweight-elasticsearch-compatible-server-2026",
         title="The best lightweight Elasticsearch-compatible servers, 2026",
         subtitle="Compatible means a different slice for each one.",
         motif=m_bars([("ELASTICSEARCH", 78319, "78,319"), ("OPENOBSERVE", 22436, "22,436"),
                       ("ZINCSEARCH", 17918, "17,918"), ("OPENSEARCH", 13892, "13,892"),
                       ("MANTICORE", 12063, "12,063"), ("QUICKWIT", 11727, "11,727"),
                       ("XERJ", 10569, "10,569")], "GITHUB STARS · 2026-10-19")),
]


def page(card: dict, number: int, total: int) -> str:
    L = bc.layout("og")
    rng = random.Random(card["slug"] + ":og")
    motif = card["motif"](L["mw"], L["mh"], rng)
    motif_svg = (f'<svg class="motif" width="{L["mw"]}" height="{L["mh"] + 4}" '
                 f'viewBox="0 0 {L["mw"]} {L["mh"] + 4}" overflow="visible" xmlns="http://www.w3.org/2000/svg">'
                 f'{motif}</svg>')
    css = bc.CSS % {**bc.TOKENS, **L, "display": bc.font_url("BigShouldersDisplay-wght.ttf"),
                    "mono": bc.font_url("JetBrainsMono-wght.ttf")}
    e = html.escape
    return f"""<!DOCTYPE html><html><head><meta charset="utf-8"><style>{css}
h1 {{ font-size: 58px; }}</style></head><body>
<div class="card">
{bc.background(L["W"], L["H"], 0)}
<div class="kicker"><span class="sq"></span><span>XERJ · BEST OF 2026 · CHECKED {DATE}</span>
<span class="no">No. {number:02d} / {total:02d}</span></div>
<div class="textcol"><div class="rule"></div><h1>{e(card["title"])}</h1><p class="sub">{e(card["subtitle"])}</p></div>
{motif_svg}
<div class="foot"><span><b>xerj.org/compare/{e(card["slug"])}</b></span><span class="r">EVERY NUMBER IS ON THE PAGE</span></div>
</div></body></html>"""


def render(cards: list[dict]) -> dict[str, object]:
    from playwright.sync_api import sync_playwright
    from PIL import Image
    out = {}
    exe = bc.chrome_path()
    if not exe:
        raise SystemExit("no Chrome/Chromium found; set CHROME")
    with sync_playwright() as p:
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
        path = LANDING / "og" / f"compare-{slug}.png"
        bc.save_png(img, path)
        print(f"wrote {path.relative_to(REPO)} ({path.stat().st_size:,} B)")
    if a.contact:
        bc.contact_sheet({s: {"og": i} for s, i in images.items()}, a.contact)
        print(f"wrote {a.contact}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
