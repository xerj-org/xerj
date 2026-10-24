#!/usr/bin/env python3
"""Render the xerj.org blog covers: one "engineering field-log poster" per post.

Each post gets three files, all drawn from code (HTML + SVG, rasterised by
headless Chromium through Playwright), never from an image model:

    landing/og/blog-<slug>.png           1200x630  og:image / twitter:image
    landing/blog/covers/<slug>-hero.webp 1600x640  the cover at the top of the post
    landing/blog/covers/<slug>-thumb.webp 640x336  the card thumbnail on /blog/

Layout, type and colour are fixed for the series; only the square-matrix motif
(scripts/seo/blog_covers/motifs.py) and the words change per post. The words
live in POSTS below. Every number a cover shows must appear in its post.

    python3 scripts/seo/blog_covers/covers.py                 # all posts
    python3 scripts/seo/blog_covers/covers.py --only the-corpus-hub
    python3 scripts/seo/blog_covers/covers.py --contact /tmp/contact.png

Needs: pip install playwright pillow, plus a Chromium (`playwright install
chromium`, or point CHROME at an installed Chrome/Chromium binary). This is an
offline authoring tool, like the og-blog render note in ../assets/: CI does
not run it, the PNG/WebP outputs are committed. See README.md.
"""

from __future__ import annotations

import argparse
import html
import io
import os
import pathlib
import random
import shutil
import sys

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent.parent.parent
LANDING = REPO / "landing"
sys.path.insert(0, str(HERE))
import motifs  # noqa: E402
from motifs import MOTIFS  # noqa: E402

# ── brand tokens: the brand book's print palette on the site's day paper ────
# Paper and rules are landing/style.css html[data-theme='day'] (the theme the
# /blog/ index is served in); ink, yellow and ochre are landing/brand.html.
# Yellow is a FILL only (a square, a bar, the one result the post is about):
# it never carries text on paper. Text and hairlines that need the accent use
# ochre, which holds contrast on paper.
TOKENS = {
    "bg": "#f6f4ee",      # day --z-bg (paper)
    "ink": "#0b0b0d",     # brand ink
    "mute": "#696762",    # day --z-mute
    "faint": "#ebe8df",   # grid paper rule
    "line": "#cfcbbf",    # day --z-line
    "accent": "#ffc400",  # brand yellow, fills only
    "ochre": "#a06800",   # brand ochre: accent text and accent strokes
    "cmp": "#6f8aa8",     # day --z-cmp, the comparison series, never XERJ
    "frame": "#d6d4cb",   # 1px og frame so the card holds its edge on white
}

# ── the series: one entry per post ───────────────────────────────────────────
# title     display face, set uppercase; keep it to the post's own title
# subtitle  yellow line under the title; a phrase lifted from the post
# date      the post's published date (pagedata.py `published`)
# motif     key into motifs.MOTIFS; the motif's docstring names its sources
# alt       alt text for the hero/thumb and og:image:alt: what the card shows
POSTS: list[dict[str, str]] = [
    dict(slug="does-xerj-beat-jev", date="2026-09-21", motif="jevbars",
         title="Does XERJ beat JEV?",
         subtitle="On the bill, outright. On FiQA, no.",
         alt="Cover art: three pairs of square-matrix bars, ink for XERJ and blue for BM25+Jev, over one long blue bar and a single yellow square for the zero-cost local run",
         og_alt="Cover: “Does XERJ beat JEV?” in display type beside the post’s square-matrix art, labelled with its figures. The art: three pairs of square-matrix bars, ink for XERJ and blue for BM25+Jev, over one long blue bar and a single yellow square for the zero-cost local run"),
    dict(slug="jev-vs-a-bm25-vote", date="2026-09-22", motif="vote",
         title="A BM25 vote ties Jev on spam and beats it on intent",
         subtitle="Count your labels before you pay for a model.",
         alt="Cover art: two grids of 500 squares, filled for correct answers; the ink grid has a band of yellow squares where it beats the blue grid",
         og_alt="Cover: “A BM25 vote ties Jev on spam and beats it on intent” in display type beside the post’s square-matrix art, labelled with its figures. The art: two grids of 500 squares, filled for correct answers; the ink grid has a band of yellow squares where it beats the blue grid"),
    dict(slug="cutting-the-index-28-percent", date="2026-09-27", motif="index28",
         title="Cutting the index 28%",
         subtitle="Dissect first, write Rust second.",
         alt="Cover art: a large ink block of squares wrapped in a yellow L-shaped band that marks the 28% cut, beside two pairs of thin before-and-after bars",
         og_alt="Cover: “Cutting the index 28%” in display type beside the post’s square-matrix art, labelled with its figures. The art: a large ink block of squares wrapped in a yellow L-shaped band that marks the 28% cut, beside two pairs of thin before-and-after bars"),
    dict(slug="one-vulnerability-three-names", date="2026-09-27", motif="names",
         title="One vulnerability, three names",
         subtitle="Checksums prove integrity. They do not prove origin.",
         alt="Cover art: three outlined boxes trailing lines of small squares that converge on one square-matrix disc with a yellow seal at its centre",
         og_alt="Cover: “One vulnerability, three names” in display type beside the post’s square-matrix art, labelled with its figures. The art: three outlined boxes trailing lines of small squares that converge on one square-matrix disc with a yellow seal at its centre"),
    dict(slug="the-rc80-gates", date="2026-09-30", motif="gates",
         title="The rc.80 gates",
         subtitle="Measured in public, losses left in.",
         alt="Cover art: ten gates built from squares in two rows: three yellow with check marks, three ink with crosses, one half yellow and three faint",
         og_alt="Cover: “The rc.80 gates” in display type beside the post’s square-matrix art, labelled with its figures. The art: ten gates built from squares in two rows: three yellow with check marks, three ink with crosses, one half yellow and three faint"),
    dict(slug="the-corpus-hub", date="2026-10-01", motif="tiles",
         title="The Corpus Hub",
         subtitle="Pinned, licenced, signed, contributed by pull request.",
         alt="Cover art: a 10 by 10 wall of square-matrix pack tiles in shades of ink, the first four solid yellow and the fifth framed",
         og_alt="Cover: “The Corpus Hub” in display type beside the post’s square-matrix art, labelled with its figures. The art: a 10 by 10 wall of square-matrix pack tiles in shades of ink, the first four solid yellow and the fifth framed"),
    dict(slug="eval-scars-from-agent-memory", date="2026-10-07", motif="scars",
         title="Six eval scars from production agent memory",
         subtitle="If every query scores zero hits, suspect your mask.",
         alt="Cover art: six clusters of diagonal scratch marks made of small squares, five in ink and one in yellow",
         og_alt="Cover: “Six eval scars from production agent memory” in display type beside the post’s square-matrix art, labelled with its figures. The art: six clusters of diagonal scratch marks made of small squares, five in ink and one in yellow"),
    dict(slug="the-exploit-hub", date="2026-10-08", motif="network",
         title="The exploit hub, tested on three fresh CVEs",
         subtitle="One query, exact attribution, wrong patch.",
         alt="Cover art: a network of small square nodes; a yellow hub linked to 36 yellow nodes, an ink node with one link, and an empty square frame with none",
         og_alt="Cover: “The exploit hub, tested on three fresh CVEs” in display type beside the post’s square-matrix art, labelled with its figures. The art: a network of small square nodes; a yellow hub linked to 36 yellow nodes, an ink node with one link, and an empty square frame with none"),
    dict(slug="corpus-hub-v2", date="2026-10-19", motif="route",
         title="Corpus Hub v2",
         subtitle="Ask the question, get the one pack that answers it.",
         alt="Cover art: a faint wall of 251 small tiles above 14 larger square-matrix tiles; a yellow route runs from the top into one yellow tile and down to a framed node",
         og_alt="Cover: “Corpus Hub v2” in display type beside the post’s square-matrix art, labelled with its figures. The art: a faint wall of 251 small tiles above 14 larger square-matrix tiles; a yellow route runs from the top into one yellow tile and down to a framed node"),
    dict(slug="rc99-smart-autoindex-harness-tools", date="2026-10-23", motif="budget",
         title="rc.99",
         subtitle="Smart, resource-aware autoindex and more harness tools.",
         alt="Cover art: eight large squares for the 8 GiB tier, the first two yellow for the 2 GiB memtable budget, beside eight small squares for the 8 MiB document ceiling and one outlined square for JSON streamed past 64 MiB",
         og_alt="Cover: “rc.99” in display type beside the post’s square-matrix art, labelled 8 GiB and 2 GiB. The art: eight large squares for the 8 GiB tier, the first two yellow for the 2 GiB memtable budget, beside eight small squares for the 8 MiB document ceiling and one outlined square for JSON streamed past 64 MiB"),
]

# og: title + motif with its labels, for social feeds that show the card alone.
# hero / thumb: art only (no title, no labels); the page around them already
# carries the title. "thumb" renders at og size and is downscaled to THUMB.
SIZES = {"og": (1200, 630), "hero": (1600, 640), "thumb": (1200, 630)}
ART_ONLY = {"hero", "thumb"}
THUMB = (640, 336)


def font_url(name: str) -> str:
    return (HERE / "fonts" / name).as_uri()


CSS = """
@font-face { font-family: 'Big Shoulders Display'; font-weight: 100 900;
             src: url('%(display)s') format('truetype'); }
@font-face { font-family: 'JetBrains Mono'; font-weight: 100 800;
             src: url('%(mono)s') format('truetype'); }
* { box-sizing: border-box; margin: 0; padding: 0; }
html, body { background: %(bg)s; }
.card { position: relative; overflow: hidden; background: %(bg)s; color: %(ink)s;
        width: %(W)spx; height: %(H)spx; }
.bgsvg { position: absolute; inset: 0; }
.kicker { position: absolute; left: %(P)spx; top: %(P)spx; right: %(P)spx;
          font: 600 %(kfs)spx/1 'JetBrains Mono', monospace; letter-spacing: .14em;
          color: %(ink)s; display: flex; gap: 14px; align-items: center; }
.kicker .sq { width: 14px; height: 14px; background: %(accent)s; flex: none; }
.kicker .no { margin-left: auto; color: %(mute)s; }
.textcol { position: absolute; left: %(P)spx; top: %(ttop)spx; width: %(tw)spx;
           height: %(th)spx; display: flex; flex-direction: column; justify-content: center; }
h1 { font-family: 'Big Shoulders Display', sans-serif; font-weight: 900;
     text-transform: uppercase; line-height: .9; letter-spacing: .005em;
     color: %(ink)s; font-size: %(tfs)spx; }
.sub { margin-top: 22px; font-family: 'Big Shoulders Display', sans-serif; font-weight: 700;
       text-transform: uppercase; letter-spacing: .02em; line-height: 1.02;
       color: %(ochre)s; font-size: %(sfs)spx; max-width: %(tw)spx; }
.rule { width: 72px; height: 1px; background: %(ink)s; margin: 0 0 22px; }
.motif { position: absolute; left: %(mx)spx; top: %(my)spx; }
.foot { position: absolute; left: %(P)spx; right: %(P)spx; bottom: %(fb)spx;
        border-top: 1px solid %(line)s; padding-top: 14px; display: flex;
        font: 500 %(ffs)spx/1 'JetBrains Mono', monospace; letter-spacing: .08em; color: %(mute)s; }
.foot b { color: %(ink)s; font-weight: 600; }
.foot .r { margin-left: auto; }
svg text { font-family: 'JetBrains Mono', monospace; }
.y { fill: %(accent)s; } .w { fill: %(ink)s; } .b { fill: %(cmp)s; }
.m { fill: %(mute)s; } .f { fill: %(line)s; }
.y-s { stroke: %(ochre)s; } .w-s { stroke: %(ink)s; } .b-s { stroke: %(cmp)s; }
.m-s { stroke: %(mute)s; } .f-s { stroke: %(line)s; }
.bgfill { fill: %(bg)s; } .fa { fill: %(faint)s; }
svg text { paint-order: stroke; stroke: %(bg)s; stroke-width: 5px; stroke-linejoin: round; }
.yo { fill: %(accent)s; stroke: %(ochre)s; stroke-width: 1px; }
.lab { font-size: %(lfs)spx; font-weight: 600; letter-spacing: .1em; fill: %(mute)s; }
.lab.y { fill: %(ochre)s; } .lab.w { fill: %(ink)s; } .lab.b { fill: %(cmp)s; }
.num { font-size: %(nfs)spx; font-weight: 600; letter-spacing: .02em; }
.num.y { fill: %(ochre)s; } .num.b { fill: %(cmp)s; } .num.w { fill: %(ink)s; } .num.m { fill: %(mute)s; }
.tag { font-size: %(gfs)spx; font-weight: 700; letter-spacing: .06em; }
.tag.y { fill: %(ochre)s; } .tag.w { fill: %(ink)s; } .tag.m { fill: %(mute)s; }
.big { font-family: 'Big Shoulders Display', sans-serif; font-weight: 900; font-size: %(bfs)spx; }
"""

FIT_ART_JS = """
() => {
  const svg = document.getElementById('art'), b = document.getElementById('artg').getBBox();
  const pad = 6;
  svg.setAttribute('viewBox', `${b.x - pad} ${b.y - pad} ${b.width + 2 * pad} ${b.height + 2 * pad}`);
  return [b.width, b.height];
}
"""

FIT_JS = """
() => {
  const h1 = document.querySelector('h1'), col = document.querySelector('.textcol');
  let fs = parseFloat(getComputedStyle(h1).fontSize);
  while (col.scrollHeight > col.clientHeight + 1 && fs > 30) { fs -= 2; h1.style.fontSize = fs + 'px'; }
  return fs;
}
"""


def layout(kind: str) -> dict:
    W, H = SIZES[kind]
    if kind == "hero":
        return dict(W=W, H=H, P=64, kfs=16, ttop=0, tw=0, th=0, tfs=0, sfs=0,
                    mx=90, my=70, mw=1140, mh=470, aw=1420, ah=500, fb=40, ffs=15, lfs=13, nfs=14, gfs=12, bfs=64)
    if kind == "thumb":
        return dict(W=W, H=H, P=56, kfs=15, ttop=0, tw=0, th=0, tfs=0, sfs=0,
                    mx=90, my=70, mw=980, mh=470, aw=1020, ah=490, fb=40, ffs=15, lfs=12, nfs=13, gfs=11, bfs=54)
    if kind == "og":
        return dict(W=W, H=H, P=56, kfs=15, ttop=104, tw=500, th=410, tfs=92, sfs=30,
                    mx=610, my=112, mw=536, mh=400, fb=40, ffs=15, lfs=12, nfs=13, gfs=11, bfs=54)
    return dict(W=W, H=H, P=64, kfs=16, ttop=110, tw=640, th=410, tfs=104, sfs=32,
                mx=790, my=112, mw=746, mh=410, fb=40, ffs=15, lfs=13, nfs=14, gfs=12, bfs=64)


def background(W: int, H: int, seed: int) -> str:
    """Grid paper (1px squares, the matrix every motif snaps to) and crop marks.
    Flat: no gradient, no grain, no shadow."""
    marks = []
    for (x, y) in [(22, 22), (W - 22, 22), (22, H - 22), (W - 22, H - 22)]:
        marks.append(f'<line x1="{x-9}" y1="{y}" x2="{x+9}" y2="{y}" stroke="{TOKENS["line"]}"/>'
                     f'<line x1="{x}" y1="{y-9}" x2="{x}" y2="{y+9}" stroke="{TOKENS["line"]}"/>')
    return f"""<svg class="bgsvg" width="{W}" height="{H}" viewBox="0 0 {W} {H}" xmlns="http://www.w3.org/2000/svg">
<defs>
 <pattern id="grid" width="24" height="24" patternUnits="userSpaceOnUse">
  <path d="M24 0H0V24" fill="none" stroke="{TOKENS["faint"]}" stroke-width="1"/></pattern>
</defs>
<rect width="{W}" height="{H}" fill="url(#grid)"/>
{''.join(marks)}
</svg>"""


def page(post: dict, kind: str, number: int, total: int) -> str:
    L = layout(kind)
    seed = sum(post["slug"].encode()) % 997
    rng = random.Random(post["slug"] + ":" + kind)
    motifs.LABELS = kind not in ART_ONLY
    try:
        motif = MOTIFS[post["motif"]](L["mw"], L["mh"], rng)
    finally:
        motifs.LABELS = True
    motif_svg = (f'<svg class="motif" width="{L["mw"]}" height="{L["mh"] + 4}" '
                 f'viewBox="0 0 {L["mw"]} {L["mh"] + 4}" overflow="visible" xmlns="http://www.w3.org/2000/svg">'
                 f'{motif}</svg>')
    css = CSS % {**TOKENS, **L, "display": font_url("BigShouldersDisplay-wght.ttf"),
                 "mono": font_url("JetBrainsMono-wght.ttf")}
    e = html.escape
    if kind in ART_ONLY:
        # the motif is drawn in its own (mw x mh) box, then FIT_ART_JS fits its
        # real bounding box, centred, into the (aw x ah) art area
        art = (f'<svg id="art" class="motif" width="{L["aw"]}" height="{L["ah"]}" '
               f'viewBox="0 0 {L["mw"]} {L["mh"]}" preserveAspectRatio="xMidYMid meet" '
               f'xmlns="http://www.w3.org/2000/svg"><g id="artg">{motif}</g></svg>')
        return f"""<!DOCTYPE html><html><head><meta charset="utf-8"><style>{css}</style></head><body>
<div class="card">
{background(L["W"], L["H"], seed)}
{art}
</div></body></html>"""
    return f"""<!DOCTYPE html><html><head><meta charset="utf-8"><style>{css}</style></head><body>
<div class="card">
{background(L["W"], L["H"], seed)}
<div class="kicker"><span class="sq"></span><span>XERJ · ENGINEERING LOG · {e(post["date"])}</span>
<span class="no">No. {number:02d} / {total:02d}</span></div>
<div class="textcol"><div class="rule"></div><h1>{e(post["title"])}</h1><p class="sub">{e(post["subtitle"])}</p></div>
{motif_svg}
<div class="foot"><span><b>xerj.org/blog/{e(post["slug"])}</b></span><span class="r">DRAWN FROM THE POST'S OWN NUMBERS</span></div>
</div></body></html>"""


def chrome_path() -> str | None:
    if os.environ.get("CHROME"):
        return os.environ["CHROME"]
    for c in ("google-chrome", "chromium", "chromium-browser"):
        p = shutil.which(c)
        if p:
            return p
    return None


def save_png(img, path: pathlib.Path) -> None:
    from PIL import Image
    q = img.convert("RGB").quantize(colors=96, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE)
    path.parent.mkdir(parents=True, exist_ok=True)
    q.save(path, optimize=True)


def save_webp(img, path: pathlib.Path) -> None:
    """Lossless WebP of the same 96-colour image the PNG gets: flat inks and
    dot screens compress far better losslessly than with lossy WebP, and the
    dots stay crisp."""
    from PIL import Image
    q = img.convert("RGB").quantize(colors=96, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE)
    path.parent.mkdir(parents=True, exist_ok=True)
    q.convert("RGB").save(path, "WEBP", lossless=True, quality=100, method=6)


def render(posts: list[dict], series_total: int, numbers: dict[str, int],
           html_dir: pathlib.Path | None) -> dict[str, dict]:
    from playwright.sync_api import sync_playwright
    from PIL import Image
    out: dict[str, dict] = {}
    with sync_playwright() as p:
        kw = {}
        exe = chrome_path()
        try:
            browser = p.chromium.launch()
        except Exception:
            if not exe:
                raise
            browser = p.chromium.launch(executable_path=exe)
        for post in posts:
            out[post["slug"]] = {}
            for kind, (W, H) in SIZES.items():
                pg = browser.new_page(viewport={"width": W, "height": H}, device_scale_factor=1)
                doc = page(post, kind, numbers[post["slug"]], series_total)
                if html_dir:
                    html_dir.mkdir(parents=True, exist_ok=True)
                    (html_dir / f"{post['slug']}-{kind}.html").write_text(doc)
                tmp = HERE / f".render-{kind}.html"
                tmp.write_text(doc)
                pg.goto(tmp.as_uri())
                pg.evaluate("document.fonts.ready")
                pg.wait_for_timeout(150)
                pg.evaluate(FIT_ART_JS if kind in ART_ONLY else FIT_JS)
                png = pg.locator(".card").screenshot(type="png")
                tmp.unlink()
                pg.close()
                out[post["slug"]][kind] = Image.open(io.BytesIO(png)).convert("RGB")
        browser.close()
    return out


def framed(img):
    """The og card gets a 1px frame in TOKENS["frame"]: paper on a white
    timeline (X, Slack, LinkedIn) otherwise loses its edge. The hero and the
    thumbnail are framed by blog.css instead."""
    from PIL import ImageDraw
    out = img.copy()
    ImageDraw.Draw(out).rectangle([0, 0, out.width - 1, out.height - 1], outline=TOKENS["frame"])
    return out


def write(images: dict[str, dict]) -> list[pathlib.Path]:
    from PIL import Image
    written = []
    for slug, imgs in images.items():
        og = LANDING / "og" / f"blog-{slug}.png"
        save_png(framed(imgs["og"]), og)
        hero = LANDING / "blog" / "covers" / f"{slug}-hero.webp"
        save_webp(imgs["hero"], hero)
        thumb = LANDING / "blog" / "covers" / f"{slug}-thumb.webp"
        # the thumbnail is a downscale, full of in-between tones: lossy wins there
        thumb.parent.mkdir(parents=True, exist_ok=True)
        imgs["thumb"].resize(THUMB, Image.LANCZOS).save(thumb, "WEBP", quality=80, method=6)
        written += [og, hero, thumb]
    return written


def contact_sheet(images: dict[str, dict], path: pathlib.Path,
                  cell: tuple[int, int] = (600, 315)) -> None:
    from PIL import Image
    cols = 3
    cw, ch = cell
    gap = 24
    rows = (len(images) + cols - 1) // cols
    sheet = Image.new("RGB", (cols * cw + (cols + 1) * gap, rows * ch + (rows + 1) * gap), (255, 255, 255))
    for n, (slug, imgs) in enumerate(images.items()):
        r, c = divmod(n, cols)
        sheet.paste(framed(imgs["og"].resize((cw, ch), Image.LANCZOS)), (gap + c * (cw + gap), gap + r * (ch + gap)))
    path.parent.mkdir(parents=True, exist_ok=True)
    sheet.save(path, optimize=True)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--only", action="append", help="render only this slug (repeatable)")
    ap.add_argument("--contact", type=pathlib.Path, help="also write a contact sheet PNG of every og card")
    ap.add_argument("--html-dir", type=pathlib.Path, help="keep the intermediate HTML pages here")
    a = ap.parse_args()
    numbers = {p["slug"]: i + 1 for i, p in enumerate(POSTS)}
    todo = [p for p in POSTS if not a.only or p["slug"] in a.only]
    if a.only and len(todo) != len(a.only):
        known = {p["slug"] for p in POSTS}
        print("unknown slug(s): " + ", ".join(s for s in a.only if s not in known), file=sys.stderr)
        return 2
    images = render(todo, len(POSTS), numbers, a.html_dir)
    for f in write(images):
        print(f"wrote {f.relative_to(REPO)} ({f.stat().st_size:,} B)")
    if a.contact:
        contact_sheet(images, a.contact)
        print(f"wrote {a.contact}")
        art = a.contact.with_name(a.contact.stem + "-art" + a.contact.suffix)
        contact_sheet({k: {"og": v["hero"]} for k, v in images.items()}, art, (600, 240))
        print(f"wrote {art}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
