#!/usr/bin/env python3
"""Render the xerj.org page covers: the blog's dot-motif poster series, for pages.

Same layout, type, inks and halftone primitives as the blog cover series
(scripts/seo/blog_covers/, PR #1918), so a share card for /docs/ and one for a
blog post look like one family. Only the words and the motif change per page,
and every number a motif draws is a number the page itself states.

    landing/og/page-<slug>.png             1200x630  og:image / twitter:image
    landing/og/covers/page-<slug>-hero.webp 1600x640  the hero figure on the page

    python3 scripts/seo/page_covers/covers.py              # every page
    python3 scripts/seo/page_covers/covers.py --only home
    python3 scripts/seo/page_covers/covers.py --contact /tmp/sheet.png

Needs the blog cover generator next to it (scripts/seo/blog_covers/, for the
fonts, tokens and primitives) or BLOG_COVERS_DIR pointing at a checkout of it,
plus `pip install playwright pillow` and a Chromium. Offline authoring tool:
CI does not run it; the outputs are committed.
"""

from __future__ import annotations

import argparse
import html
import io
import os
import pathlib
import random
import sys

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent.parent.parent
LANDING = REPO / "landing"
BLOG = pathlib.Path(os.environ.get("BLOG_COVERS_DIR", HERE.parent / "blog_covers"))
if not (BLOG / "covers.py").is_file():
    sys.exit(f"page_covers: needs scripts/seo/blog_covers (PR #1918) or BLOG_COVERS_DIR; not found at {BLOG}")
sys.path.insert(0, str(BLOG))
import covers as blog  # noqa: E402
from motifs import dot, ring, label, hair, halftone, dotbar, dotpath, disc  # noqa: E402

# ── motifs: every number is quoted from the page the cover belongs to ────────


def m_ask(w, h, rng):
    """home: `xerj "how does lz4 frame format work"` captured on rc.97,
    2026-10-19: the router picks lz4 out of the 14 corpora of the signed
    catalogue, pulls its 4.4 MB pack once, starts a node on :9300 and answers
    with lz4/lib/lz4frame.h:186 among its passages (and lz4/lib/lz4.c:1481 for
    the --def LZ4_compress_default lookup). 1.75 s for the first ask
    (node start + download), 0.022 s for the next lookup."""
    slugs = ["act", "age", "agentops", "aiohttp", "alertmgr", "arkworks", "arroyo",
             "astro", "attack…", "autorag", "avro", "lz4", "snappy", "zstd"]
    out = [label(0, 16, 'xerj "how does lz4 frame format work"', "num w")]
    tc, gap = 7, 9
    tw = (w - (tc - 1) * gap) / tc
    th = tw * 0.5
    y0 = 44
    hot = None
    for k, s in enumerate(slugs):
        i, j = k % tc, k // tc
        x, y = i * (tw + gap), y0 + j * (th + 24)
        is_hot = s == "lz4"
        out.append(halftone(x, y, tw, th, tw / 8,
                            (lambda u, v: 0.4 + 0.6 * (1 - v)) if is_hot else (lambda u, v: 0.14),
                            "y" if is_hot else "w", rng))
        out.append(label(x, y + th + 14, s, "tag y" if is_hot else "tag m"))
        if is_hot:
            hot = (x + tw / 2, y + th)
    out.append(label(w, y0 - 10, "14 CORPORA · SIGNED CATALOGUE", "lab", "end"))
    ny = y0 + 2 * (th + 24) + 70
    out.append(dotpath([(hot[0], hot[1] + 20), (hot[0], ny - 30)], 7, 1.8, 3, "y"))
    out.append(disc(hot[0], ny, 26, 6, "y"))
    out.append(ring(hot[0], ny, 34, "y", 1.6))
    out.append(label(hot[0] + 46, ny - 6, "4.4 MB PACK, ONCE", "lab y"))
    out.append(label(hot[0] + 46, ny + 12, "LOCAL NODE :9300", "lab w"))
    ay = ny + 70
    out.append(dotpath([(hot[0] - 34, ny + 10), (12, ay - 6)], 7, 3, 1.8, "y"))
    out.append(label(0, ay + 12, "lz4/lib/lz4frame.h:186", "big y"))
    out.append(label(0, h - 2, "1.75 s FIRST ASK · 0.022 s NEXT LOOKUP · rc.97", "lab"))
    return "".join(out)


def m_tokens(w, h, rng):
    """use-cases: output tokens per arm in the reference-coding case study
    (8 tasks, 16 runs per arm): memory only 260,916; grep-driven 26,477;
    XERJ 9,982. Bars are linear in tokens, so XERJ's is 3.8% of memory's."""
    arms = [("FROM MEMORY", 260916, "b", "11/16 SOLVED"), ("GREP-DRIVEN", 26477, "b", "16/16 SOLVED"),
            ("XERJ", 9982, "y", "16/16 SOLVED")]
    out = []
    top = 40
    span = w
    for k, (name, n, ink, solved) in enumerate(arms):
        y = top + k * 112
        out.append(label(0, y, name, "lab " + ("y" if ink == "y" else "w")))
        L = max(8 * 3, span * n / 260916)
        out.append(dotbar(0, y + 10, L, 6, 8, ink, fade=4))
        out.append(label(min(L + 12, w - 150) if L < w - 160 else w, y + 40, f"{n:,}",
                         "num " + ink, "start" if L < w - 160 else "end"))
        out.append(label(w, y, solved, "lab", "end"))
    out.append(label(0, h - 2, "OUTPUT TOKENS · 8 TASKS · 16 RUNS PER ARM · 2.7× FEWER THAN GREP", "lab"))
    return "".join(out)


def m_docs(w, h, rng):
    """docs: the four-step ladder of the docs index, step width = the number
    of distinct pages the index links under it (Get started 4; Guides 21 =
    the recipes hub, Corpus Hub, Migrate from ES, 13 recipes, 5 playbooks;
    Reference 15 = reference 5, data model 4, engine 6; Operate 4)."""
    steps = [("GET STARTED", 4, "y"), ("GUIDES", 21, "w"), ("REFERENCE", 15, "w"), ("OPERATE", 4, "w")]
    out = []
    sh = (h - 60) / len(steps)
    unit = (w - 200) / 21
    for k, (name, n, ink) in enumerate(steps):
        y = 20 + k * sh
        x = k * 40
        out.append(halftone(x, y, max(unit * n, 28), sh - 26, 9,
                            (lambda u, v: 0.85) if ink == "y" else (lambda u, v: 0.32 + 0.3 * (1 - u)),
                            ink, rng))
        out.append(label(x + max(unit * n, 28) + 14, y + 20, name, "lab " + ink))
        out.append(label(x + max(unit * n, 28) + 14, y + 40, f"{n} PAGE{'S' if n != 1 else ''}", "lab"))
    out.append(label(0, h - 2, "INSTALL → ASK → INDEX → CONNECT", "lab"))
    return "".join(out)


def m_tools(w, h, rng):
    """for-agents: the 17 tools `xerj mcp` lists over tools/list on rc.97
    (2026-10-19), grouped as the page groups them."""
    groups = [("SEARCH", 4, "w"), ("MEMORY", 2, "w"), ("BRAIN", 4, "w"),
              ("MAP · CODE · PLAN", 3, "w"), ("HUB", 3, "y"), ("FIELD REPORT", 1, "w")]
    out = []
    R = 20
    pitch = 2 * R + 16
    y = 40
    for name, n, ink in groups:
        for i in range(n):
            cx = R + 4 + i * pitch
            out.append(disc(cx, y + R, R, 6, ink, (lambda d: 1 - d * d) if ink == "y" else (lambda d: 0.55 * (1 - d * d))))
        out.append(label(4 * pitch + 10, y + R + 5, f"{n} · {name}", "lab " + ink))
        y += 2 * R + 14
    out.append(label(0, 20, "xerj mcp · tools/list", "num w"))
    out.append(label(0, h - 2, "17 MCP TOOLS · ONE BINARY · STDIO", "lab"))
    return "".join(out)


def m_arch(w, h, rng):
    """architecture: the one binary and its four surfaces as the page lists
    them: the node's Elasticsearch-compatible API on :9200, the node `xerj
    ask` starts on :9300 when nothing answers on :9200, MCP over stdio, and
    signed packs pulled from GitHub Releases."""
    cx, cy = w * 0.36, h * 0.48
    out = [disc(cx, cy, 92, 8, "y"), ring(cx, cy, 104, "y", 1.4),
           label(cx, cy + 10, "xerj", "big w", "middle")]
    ends = [((w - 4, 40), ":9200 · ES-COMPATIBLE API", "w"),
            ((w - 4, 150), ":9300 · NODE xerj ask STARTS", "y"),
            ((w - 4, 260), "stdio · MCP SERVER", "w"),
            ((w - 4, 370), "SIGNED PACKS ← RELEASES", "w")]
    for (ex, ey), text, ink in ends:
        out.append(dotpath([(cx + 110, cy), (cx + 170, ey), (ex - 270, ey)], 8, 2.6, 1.6, ink))
        out.append(label(ex, ey + 5, text, "lab " + ink, "end"))
    out.append(label(0, h - 2, "ONE BINARY · SINGLE-NODE · APACHE-2.0", "lab"))
    return "".join(out)


def m_gates(w, h, rng):
    """security: the four checks the page documents, all on by default:
    loopback bind (127.0.0.1), API-key auth unless --insecure, the
    installer's fail-closed SHA-256 check, ed25519-signed packs."""
    gates = ["127.0.0.1", "API KEY", "SHA-256", "ED25519"]
    subs = ["BIND", "AUTH ON", "INSTALLER", "PACKS"]
    out = []
    gw = (w - 3 * 24) / 4
    for k, (g, s) in enumerate(zip(gates, subs)):
        x = k * (gw + 24)
        out.append(halftone(x, 40, gw, h - 150, 10, lambda u, v: 0.2 + 0.7 * (1 - v), "y" if k == 0 else "w", rng))
        out.append(ring(x + gw / 2, 40 + (h - 150) / 2, gw * 0.28, "y", 2))
        out.append(label(x, h - 80, g, "num y"))
        out.append(label(x, h - 58, s, "lab"))
    out.append(label(0, h - 2, "ON BY DEFAULT · --insecure TURNS AUTH AND TLS OFF", "lab"))
    return "".join(out)


def m_free(w, h, rng):
    """pricing: the binary is free under Apache-2.0; the page's one cost line
    is your own hardware."""
    out = [disc(w * 0.30, h * 0.45, 150, 9, "y", lambda d: 0.9 * (1 - d ** 3)),
           label(w * 0.30, h * 0.45 + 34, "$0", "big w", "middle")]
    rows = ["BINARY · APACHE-2.0", "CORPUS PACKS · GITHUB RELEASES", "MCP SERVER · SAME BINARY", "YOUR HARDWARE · YOURS"]
    for k, r in enumerate(rows):
        y = 70 + k * 80
        out.append(dotbar(w * 0.62, y, w * 0.36, 2, 7, "w" if k < 3 else "m", fade=3))
        out.append(label(w * 0.62, y - 8, r, "lab " + ("y" if k < 3 else "")))
    out.append(label(0, h - 2, "NO TIERS · NO SIGNUP · NO API KEY", "lab"))
    return "".join(out)


# ── the series ────────────────────────────────────────────────────────────────
PAGES = [
    dict(slug="home", path="", motif=m_ask, kicker="XERJ · LOCAL SEARCH FOR AI AGENTS",
         title="Ask a repo. Get the line.", subtitle="One binary. Signed packs. Answers on your machine.",
         alt="Cover: a question routed to the lz4 tile among the 14 corpora of the signed catalogue, its 4.4 MB pack pulled once to a local node on port 9300, answering with lz4/lib/lz4frame.h:186; 1.75 s first ask, 0.022 s next lookup"),
    dict(slug="docs", path="docs/", motif=m_docs, kicker="XERJ · DOCUMENTATION",
         title="The docs", subtitle="Install, ask, index, connect.",
         alt="Cover: the XERJ docs ladder, Get started, Guides, Reference and Operate, drawn as rows of dots"),
    dict(slug="use-cases", path="use-cases", motif=m_tokens, kicker="XERJ · USE CASES",
         title="What agents use it for", subtitle="2.7× fewer output tokens than grep.",
         alt="Cover: three dot-bars of output tokens from the reference-coding case study: 260,916 from memory, 26,477 grep-driven, 9,982 with XERJ"),
    dict(slug="for-agents", path="for-agents", motif=m_tools, kicker="XERJ · FOR AI AGENTS",
         title="Built for agents", subtitle="llms.txt, 17 MCP tools, one prompt.",
         alt="Cover: 17 halftone dots, one per MCP tool, grouped search, memory, brain, map/code/plan, hub (in yellow) and field report"),
    dict(slug="architecture", path="architecture/", motif=m_arch, kicker="XERJ · ARCHITECTURE",
         title="One binary", subtitle="A node, a hub client, an MCP server.",
         alt="Cover: a yellow halftone disc labelled xerj with dotted paths to port 9200, port 9300, the MCP stdio server and signed packs from GitHub Releases"),
    dict(slug="security", path="security/", motif=m_gates, kicker="XERJ · SECURITY MODEL",
         title="Closed by default", subtitle="Loopback, auth, verified bytes.",
         alt="Cover: four dot-screen gates labelled 127.0.0.1 bind, API key auth on, SHA-256 installer and ed25519 packs"),
    dict(slug="pricing", path="pricing/", motif=m_free, kicker="XERJ · PRICING",
         title="Free", subtitle="Apache-2.0. No tiers.",
         alt="Cover: a large yellow halftone disc reading $0 beside four rows: binary Apache-2.0, corpus packs on GitHub Releases, MCP server in the same binary, your hardware"),
]


def page(p: dict, kind: str, number: int, total: int) -> str:
    L = blog.layout(kind)
    seed = sum(p["slug"].encode()) % 997
    rng = random.Random("page:" + p["slug"] + ":" + kind)
    motif = p["motif"](L["mw"], L["mh"], rng)
    R = max(L["mw"], L["mh"]) * 0.62
    wash = disc(L["mw"] * 0.62, L["mh"] * 0.50, R, 13, "fa", lambda d: 0.55 * (1 - d) ** 1.4)
    motif_svg = (f'<svg class="motif" width="{L["mw"]}" height="{L["mh"] + 4}" '
                 f'viewBox="0 0 {L["mw"]} {L["mh"] + 4}" overflow="visible" xmlns="http://www.w3.org/2000/svg">'
                 f'<g>{wash}</g><g filter="url(#rough)">{motif}</g>'
                 f'<defs><filter id="rough"><feTurbulence type="fractalNoise" baseFrequency="0.6" seed="{seed}" result="n"/>'
                 f'<feDisplacementMap in="SourceGraphic" in2="n" scale="1.6"/></filter></defs></svg>')
    css = blog.CSS % {**blog.TOKENS, **L, "display": blog.font_url("BigShouldersDisplay-wght.ttf"),
                      "mono": blog.font_url("JetBrainsMono-wght.ttf")}
    e = html.escape
    return f"""<!DOCTYPE html><html><head><meta charset="utf-8"><style>{css}</style></head><body>
<div class="card">
{blog.background(L["W"], L["H"], seed)}
<div class="kicker"><span class="sq"></span><span>{e(p["kicker"])}</span>
<span class="no">No. {number:02d} / {total:02d}</span></div>
<div class="textcol"><div class="rule"></div><h1>{e(p["title"])}</h1><p class="sub">{e(p["subtitle"])}</p></div>
{motif_svg}
<div class="foot"><span><b>xerj.org/{e(p["path"])}</b></span><span class="r">DRAWN FROM THE PAGE'S OWN NUMBERS</span></div>
{blog.grain_overlay(L["W"], L["H"], seed)}
</div></body></html>"""


def render(pages):
    from playwright.sync_api import sync_playwright
    from PIL import Image
    out = {}
    numbers = {p["slug"]: i + 1 for i, p in enumerate(PAGES)}
    with sync_playwright() as pw:
        exe = blog.chrome_path()
        try:
            browser = pw.chromium.launch()
        except Exception:
            browser = pw.chromium.launch(executable_path=exe)
        for p in pages:
            out[p["slug"]] = {}
            for kind, (W, H) in blog.SIZES.items():
                pg = browser.new_page(viewport={"width": W, "height": H}, device_scale_factor=1)
                tmp = BLOG / f".render-page-{kind}.html"
                tmp.write_text(page(p, kind, numbers[p["slug"]], len(PAGES)))
                pg.goto(tmp.as_uri())
                pg.evaluate("document.fonts.ready")
                pg.wait_for_timeout(150)
                pg.evaluate(blog.FIT_JS)
                png = pg.locator(".card").screenshot(type="png")
                tmp.unlink()
                pg.close()
                out[p["slug"]][kind] = Image.open(io.BytesIO(png)).convert("RGB")
        browser.close()
    return out


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--only", action="append")
    ap.add_argument("--contact", type=pathlib.Path)
    a = ap.parse_args()
    todo = [p for p in PAGES if not a.only or p["slug"] in a.only]
    imgs = render(todo)
    for slug, im in imgs.items():
        og = LANDING / "og" / f"page-{slug}.png"
        blog.save_png(im["og"], og)
        hero = LANDING / "og" / "covers" / f"page-{slug}-hero.webp"
        blog.save_webp(im["hero"], hero)
        for f in (og, hero):
            print(f"wrote {f.relative_to(REPO)} ({f.stat().st_size:,} B)")
    if a.contact:
        blog.contact_sheet(imgs, a.contact)
        print(f"wrote {a.contact}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
