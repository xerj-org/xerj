#!/usr/bin/env python3
"""Generate the site-wide Open Graph card, `landing/og/xerj-card.png`.

Why this exists at all: zero pages had an `og:image`, no raster brand asset
was in the repo, and the brand SVGs (`landing/brandbook/*.svg`) are unusable
as OG images for two independent reasons — social crawlers do not render
SVG, and those files draw their letterforms with `<text>` in a webfont the
crawler does not have.

Why it is hand-rasterised: the toolchain here is Python 3 stdlib only.  No
Pillow, no cairo, no rsvg-convert, no headless browser.  So this module ships
a tiny scanline polygon rasteriser with 4× vertical supersampling and exact
horizontal span coverage (good antialiasing), a set of heavy condensed caps
described as polygons, and a stdlib zlib/struct PNG writer.

The design is a direct transcription of `landing/brandbook/xerj-wordmark-night.svg`
into the 1200×630 OG frame: paper `XERJ`, accent `.AI`, one 1px paper rule
under the wordmark and one 1px accent rule over `.AI`, on `--z-bg`.  Colours
are the literal design tokens from `landing/style.css`.

Deterministic: same output bytes on every run, so `--check` can gate CI.

    python3 scripts/seo/mk_og_card.py --write
    python3 scripts/seo/mk_og_card.py --check
"""

from __future__ import annotations

import argparse
import math
import pathlib
import struct
import sys
import zlib

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import urlmap  # noqa: E402

# ── brand tokens (landing/style.css :root) ──────────────────────────────────

BG = (0x0B, 0x0B, 0x0D)      # --z-bg
INK = (0xF4, 0xF2, 0xEC)     # --z-ink
MUTE = (0x8A, 0x86, 0x80)    # --z-mute
ACCENT = (0xFF, 0xC4, 0x00)  # --z-accent

WIDTH, HEIGHT = 1200, 630

# ── glyph set ───────────────────────────────────────────────────────────────
#
# Heavy condensed caps, in a 100-unit cap-height box with y=0 at the cap line
# and y=100 on the baseline.  Each entry is (advance_width, [polygon, ...]);
# a polygon is a list of (x, y) points.  Polygons are combined by summed,
# clamped coverage rather than a winding rule: that keeps the crossing strokes
# of X and A solid AND leaves no seam where two rectangles merely abut.
#
# Only the characters the card actually needs are defined.

def _rect(x0: float, y0: float, x1: float, y1: float) -> list[tuple[float, float]]:
    return [(x0, y0), (x1, y0), (x1, y1), (x0, y1)]


GLYPHS: dict[str, tuple[float, list[list[tuple[float, float]]]]] = {
    " ": (34, []),
    ".": (24, [_rect(0, 78, 22, 100)]),
    "A": (78, [
        [(28, 0), (50, 0), (22, 100), (0, 100)],
        [(28, 0), (50, 0), (78, 100), (56, 100)],
        _rect(13, 60, 65, 78),
    ]),
    "C": (62, [_rect(0, 0, 62, 20), _rect(0, 0, 22, 100), _rect(0, 80, 62, 100)]),
    "E": (62, [
        _rect(0, 0, 22, 100), _rect(22, 0, 62, 20),
        _rect(22, 40, 56, 60), _rect(22, 80, 62, 100),
    ]),
    "F": (58, [_rect(0, 0, 22, 100), _rect(22, 0, 58, 20), _rect(22, 40, 52, 60)]),
    "G": (70, [
        _rect(0, 0, 70, 20), _rect(0, 0, 22, 100), _rect(0, 80, 70, 100),
        _rect(48, 52, 70, 100), _rect(38, 52, 70, 70),
    ]),
    "H": (66, [_rect(0, 0, 22, 100), _rect(44, 0, 66, 100), _rect(22, 40, 44, 60)]),
    "I": (22, [_rect(0, 0, 22, 100)]),
    "J": (56, [_rect(34, 0, 56, 100), _rect(0, 78, 56, 100), _rect(0, 64, 22, 100)]),
    "N": (70, [
        _rect(0, 0, 22, 100), _rect(48, 0, 70, 100),
        [(0, 0), (22, 0), (70, 100), (48, 100)],
    ]),
    "O": (70, [
        _rect(0, 0, 70, 20), _rect(0, 80, 70, 100),
        _rect(0, 0, 22, 100), _rect(48, 0, 70, 100),
    ]),
    "R": (72, [
        _rect(0, 0, 22, 100), _rect(22, 0, 52, 18), _rect(48, 0, 70, 58),
        _rect(22, 40, 52, 58), [(44, 58), (66, 58), (72, 100), (50, 100)],
    ]),
    "S": (62, [
        _rect(0, 0, 62, 20), _rect(0, 0, 22, 48), _rect(0, 40, 62, 58),
        _rect(40, 40, 62, 100), _rect(0, 80, 62, 100),
    ]),
    "T": (62, [_rect(0, 0, 62, 20), _rect(20, 0, 42, 100)]),
    "X": (78, [
        [(0, 0), (24, 0), (78, 100), (54, 100)],
        [(54, 0), (78, 0), (24, 100), (0, 100)],
    ]),
}

SIDE_BEARING = 0.10  # of cap height, added after every glyph


# ── rasteriser ──────────────────────────────────────────────────────────────


class Canvas:
    """RGB canvas with summed-coverage polygon fill and alpha compositing."""

    def __init__(self, w: int, h: int, bg: tuple[int, int, int]):
        self.w, self.h = w, h
        self.px = bytearray(bg * (w * h))

    def fill_polys(self, polys, color: tuple[int, int, int], ss: int = 4) -> None:
        cov = self._coverage(polys, ss)
        r, g, b = color
        for (y, x), a in cov.items():
            i = (y * self.w + x) * 3
            inv = 1.0 - a
            self.px[i] = int(self.px[i] * inv + r * a + 0.5)
            self.px[i + 1] = int(self.px[i + 1] * inv + g * a + 0.5)
            self.px[i + 2] = int(self.px[i + 2] * inv + b * a + 0.5)

    def _coverage(self, polys, ss: int) -> dict[tuple[int, int], float]:
        acc: dict[tuple[int, int], float] = {}
        for pts in polys:
            if len(pts) < 3:
                continue
            ys = [p[1] for p in pts]
            y0 = max(0, int(math.floor(min(ys))))
            y1 = min(self.h - 1, int(math.ceil(max(ys))))
            n = len(pts)
            for py in range(y0, y1 + 1):
                row: dict[int, float] = {}
                for s in range(ss):
                    sy = py + (s + 0.5) / ss
                    xs = []
                    for i in range(n):
                        ax, ay = pts[i]
                        bx, by = pts[(i + 1) % n]
                        if (ay <= sy < by) or (by <= sy < ay):
                            xs.append(ax + (sy - ay) / (by - ay) * (bx - ax))
                    xs.sort()
                    for i in range(0, len(xs) - 1, 2):
                        xa, xb = xs[i], xs[i + 1]
                        if xb <= 0 or xa >= self.w:
                            continue
                        xa, xb = max(xa, 0.0), min(xb, float(self.w))
                        for px in range(int(xa), min(int(math.ceil(xb)), self.w)):
                            lo, hi = max(xa, px), min(xb, px + 1.0)
                            if hi > lo:
                                row[px] = row.get(px, 0.0) + (hi - lo) / ss
                for px, v in row.items():
                    k = (py, px)
                    acc[k] = min(1.0, acc.get(k, 0.0) + v)
        return acc

    def rule(self, x0: float, y: float, x1: float, color, weight: float = 2.0) -> None:
        self.fill_polys([_rect(x0, y, x1, y + weight)], color)

    def to_png(self) -> bytes:
        raw = bytearray()
        stride = self.w * 3
        for y in range(self.h):
            raw.append(0)  # filter: None
            raw += self.px[y * stride:(y + 1) * stride]

        def chunk(tag: bytes, data: bytes) -> bytes:
            return (struct.pack(">I", len(data)) + tag + data
                    + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF))

        ihdr = struct.pack(">IIBBBBB", self.w, self.h, 8, 2, 0, 0, 0)
        return (b"\x89PNG\r\n\x1a\n"
                + chunk(b"IHDR", ihdr)
                + chunk(b"IDAT", zlib.compress(bytes(raw), 9))
                + chunk(b"IEND", b""))


# ── text layout ─────────────────────────────────────────────────────────────


def text_width(s: str, cap: float, track: float = 0.0) -> float:
    total = 0.0
    for ch in s:
        adv, _ = GLYPHS[ch.upper()]
        total += (adv / 100.0 + SIDE_BEARING) * cap + track
    return total - track if s else 0.0


def draw_text(c: Canvas, s: str, x: float, baseline: float, cap: float,
              color, track: float = 0.0) -> float:
    polys = []
    pen = x
    for ch in s:
        adv, shapes = GLYPHS[ch.upper()]
        k = cap / 100.0
        for poly in shapes:
            polys.append([(pen + px * k, baseline - cap + py * k) for px, py in poly])
        pen += (adv / 100.0 + SIDE_BEARING) * cap + track
    c.fill_polys(polys, color)
    return pen - track


# ── the card ────────────────────────────────────────────────────────────────


def render() -> bytes:
    c = Canvas(WIDTH, HEIGHT, BG)

    cap = 190.0
    baseline = 372.0
    left = text_width("XERJ", cap)
    right = text_width(".AI", cap)
    x0 = (WIDTH - (left + right)) / 2.0
    split = x0 + left  # the exact start of the "." glyph — both rules anchor here

    draw_text(c, "XERJ", x0, baseline, cap, INK)
    draw_text(c, ".AI", split, baseline, cap, ACCENT)

    # Brandbook geometry: paper rule under XERJ, accent rule over .AI.
    c.rule(x0, baseline + 44, split, INK, 2.0)
    c.rule(split, baseline - cap - 46, x0 + left + right, ACCENT, 2.0)

    tag = "SEARCH FOR THE AGENT ERA"
    tcap, ttrack = 30.0, 9.0
    tw = text_width(tag, tcap, ttrack)
    draw_text(c, tag, (WIDTH - tw) / 2.0, 520.0, tcap, MUTE, ttrack)

    return c.to_png()


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--write", action="store_true", help="write the PNG")
    ap.add_argument("--check", action="store_true",
                    help="exit 1 if the committed PNG differs from the render")
    args = ap.parse_args(argv)

    out = urlmap.landing_dir() / urlmap.OG_IMAGE_PATH
    png = render()

    if args.check:
        if not out.exists():
            print(f"FAIL {out} is missing; run mk_og_card.py --write", file=sys.stderr)
            return 1
        if out.read_bytes() != png:
            print(f"FAIL {out} differs from the generated card", file=sys.stderr)
            return 1
        print(f"ok   {out} matches ({len(png)} bytes)")
        return 0

    if args.write:
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_bytes(png)
        print(f"wrote {out} ({len(png)} bytes, {WIDTH}x{HEIGHT})")
        return 0

    ap.print_help()
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
