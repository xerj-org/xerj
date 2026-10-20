"""Build-time treemap for /contributors/ (stdlib only, no JS needed).

Two modes share this code: People (one tile per contributor, sized by the
GitHub stars on their repositories, contributors.volume()["gh_stars"]) and
Repos (one tile per distinct pinned repository with a recorded star count).

Readability rule: every tile shows its label on one line at >= 12px.

Band 0 (the top) is a classic squarified treemap (Bruls/Huizing/van Wijk,
run in a space stretched by ASPECT so tiles come out wide enough for a
name): area is proportional to stars, and K is the largest top-K for which
every tile fits its label. Tiers: xl avatar + name (16-20px) + stars;
m name + stars, with a 32px avatar when it fits; s name at 14px. Widths are
estimated with a deliberately wide glyph (0.62 em per character). Three
design widths are picked by a container query; positions are % of an
aspect-ratio box and the design width is the smallest it is shown at, so
tiles only grow.

The stars span five orders of magnitude, so one proportional map holds only
~46 readable names. Below band 0 the next ranks (up to the layout's cap) flow
in bands of rows (TAIL_BANDS: >=10k stars name + stars + avatar, 1k-10k name,
below 1k the compact 12px login or repo name). A tail tile is never narrower
than its label (flex-basis auto, nowrap) and the rest of each row is shared
in proportion to stars (flex-grow), so within a band more stars = wider.
Last comes one "N others" tile that opens the table filtered to them.
No borders, shadows or gradients: GAP px of page background separates tiles.
"""

GAP = 2                 # px of page background right of / below each tile; no borders
PAD_X = 8               # horizontal padding inside a tile, each side
MIN_FONT = 14           # px: smallest name on the s tier and up
XS_FONT, XS_PAD_X, XS_PAD_Y = 12, 4, 3             # compact tier for the long tail
LINE = 1.25             # line-height of tile text
GLYPH = 0.62            # em per character; wider than IBM Plex Sans 600 averages (~0.55)
PAD_Y = 6               # vertical padding inside a tile, each side
BORDER = 0              # tiles have no border: the GAP separates them
CHROME = 2 * (PAD_Y + BORDER)                      # vertical px a tile spends on padding + border
MIN_H = int(MIN_FONT * LINE + CHROME + 0.999) + 1  # one 14px line, rounded up, +1px slack
XS_H = int(XS_FONT * LINE + 2 * XS_PAD_Y + 0.999) + 1
INLINE_AV, INLINE_GAP = 32, 8                      # avatar beside the two-line label
OTHERS_H = 104          # px height of the final "N others" tile

# (name, design width, band-0 height, tile aspect, tiles shown in all, container range)
LAYOUTS = (
    ("l", 1040, 720, 2.5, 800, "(min-width:1040px)"),
    ("m", 680, 620, 3.0, 800, "(min-width:680px) and (max-width:1039.98px)"),
    ("s", 320, 540, 3.0, 400, "(max-width:679.98px)"),
)

# Light tints; --z-ink #1a1a1a and --z-mute #4a4843 are both >= 7:1 on each
# (WCAG AAA for normal text). test_gen.TreemapTest checks the ratios.
TINTS = ("#fff1c7", "#ebe4f7", "#dfebf5", "#e2f0e4", "#f7e6de", "#eeebe3")
OTHER_TINT = "#e7e5de"
INK, MUTE = "#1a1a1a", "#4a4843"


def text_w(s, px, pad=PAD_X):
    return len(s) * px * GLYPH + 2 * pad


def _worst(row, short):
    s = sum(row)
    return max(max(row) * short * short / (s * s), (s * s) / (short * short * min(row)))


def squarify(values, w, h, aspect=1.0):
    """squarify() toward tiles of `aspect`:1 (width:height): lay out in a space
    narrowed by `aspect`, then stretch x back. Areas are unchanged."""
    return [(x * aspect, y, rw * aspect, rh) for x, y, rw, rh in _squarify(values, w / aspect, h)]


def _squarify(values, w, h):
    """Bruls/Huizing/van Wijk squarified layout of `values` (sorted descending,
    all > 0) into a w x h rectangle. Returns [(x, y, w, h)] in input order."""
    total = float(sum(values))
    sizes = [v * w * h / total for v in values]
    out, x, y = [], 0.0, 0.0
    i = 0
    while i < len(sizes):
        short = min(w, h)
        row = [sizes[i]]
        j = i + 1
        while j < len(sizes) and _worst(row + [sizes[j]], short) <= _worst(row, short):
            row.append(sizes[j])
            j += 1
        s = sum(row)
        if w >= h:                      # vertical strip on the left
            rw = s / h if h else 0
            yy = y
            for a in row:
                rh = a / rw if rw else 0
                out.append((x, yy, rw, rh))
                yy += rh
            x += rw
            w -= rw
        else:                           # horizontal strip on top
            rh = s / w if w else 0
            xx = x
            for a in row:
                rw_ = a / rh if rh else 0
                out.append((xx, y, rw_, rh))
                xx += rw_
            y += rh
            h -= rh
        i = j
    return out


def _label(item, w, h, compact=True):
    """What a w x h tile (already inset by GAP) shows, or None if not even
    the shortest label fits. item["names"]: labels, preferred first."""
    if h < XS_H:
        return None
    names, stars, star_h = item["names"], item["stars_txt"], 14 * LINE
    if h >= MIN_H:
        if w >= 150 and h >= 132 and item.get("avatar"):   # big: avatar + name + stars
            av = int(max(40, min(104, min(w, h) * 0.42)))
            for fs in (20, 18, 16):
                for nm in names:
                    if text_w(nm, fs) <= w and text_w(stars, 14) <= w and av + 2 + fs * LINE + 2 + star_h + CHROME + 1 <= h:
                        return {"tier": "xl", "text": nm, "fs": fs, "av": av}
        for fs in (16, 14):             # two lines: name + stars, a small avatar beside them if it fits
            if fs * LINE + 2 + star_h + CHROME + 1 <= h and text_w(stars, 14) <= w:
                for nm in names:
                    if text_w(nm, fs) <= w:
                        room = w - max(text_w(nm, fs), text_w(stars, 14))
                        av = INLINE_AV if item.get("avatar") and room >= INLINE_AV + INLINE_GAP else 0
                        return {"tier": "m", "text": nm, "fs": fs, "av": av}
        for nm in names:                # one line at 14px
            if text_w(nm, MIN_FONT) <= w:
                return {"tier": "s", "text": nm, "fs": MIN_FONT}
    if compact:
        for nm in sorted(names, key=len):   # compact: the shortest label at 12px
            if text_w(nm, XS_FONT, XS_PAD_X) <= w:
                return {"tier": "xs", "text": nm, "fs": XS_FONT}
    return None


def _fits(items, w, h, aspect, compact):
    rects = squarify([it["stars"] for it in items], w, h, aspect)
    labels = [_label(it, r[2] - GAP, r[3] - GAP, compact) for it, r in zip(items, rects)]
    return (list(zip(items, rects, labels)) if all(labels) else None)


def fit(items, w, h, aspect=1.0, cap=300, compact=False):
    """Band 0: the top K items (stars > 0) for the largest K <= cap at which
    every tile's label fits a w x h box. [(item, (x,y,w,h), label)]."""
    pos = [it for it in items if it["stars"] > 0]
    for k in range(min(len(pos), cap), 0, -1):
        got = _fits(pos[:k], w, h, aspect, compact)
        if got:
            return got
    return []


def _pct(v, total):
    return f"{100.0 * v / total:.3f}".rstrip("0").rstrip(".") + "%"


def _short(n):
    for div, suf in ((1_000_000, "M"), (1_000, "k")):
        if n >= div:
            return f"{n / div:.1f}".rstrip("0").rstrip(".") + suf
    return f"{n:,}"


# The tail below band 0: rows of tiles that wrap like text. Each tile is at
# least as wide as its label (flex-basis auto, nowrap), and the spare width
# in a row is shared out in proportion to stars (flex-grow), so within a band
# a tile with 4x the stars is visibly wider, and no label is ever squeezed.
# Bands (TAIL_BANDS) set the row height and label tier by star range.
TAIL_BANDS = ((10000, "m"), (1000, "s"), (100, "xs"), (0, "xs"))


def _tail_label(it, tier):
    if tier == "xs":
        return min(it["names"], key=len)
    return next((n for n in it["names"] if len(n) <= 24), min(it["names"], key=len))


def render(items, esc, mode="people", caption=lambda info: "", total=None):
    """The map for one mode: band 0 (three absolute squarified layouts) plus
    the flow tail (one HTML for all widths; container queries hide the
    ranks a layout's band 0 already shows, and ranks past its cap), plus
    the "N others" tile. Returns (html, info) with info[key] = (top, shown,
    others). items: dicts with names, stars, stars_txt, href, avatar, title,
    sorted by stars desc (rank = position + 1)."""
    total = len(items) if total is None else total
    pos = [it for it in items if it["stars"] > 0]
    info, tops = {}, {}
    for key, W, H, aspect, cap, _ in LAYOUTS:
        top = fit(pos, W, H, aspect)
        shown = min(cap, len(pos))
        tops[key] = (W, H, top)
        info[key] = (len(top), shown, total - shown)
    kmin = min(v[0] for v in info.values())
    cap_max = max(v[1] for v in info.values())
    out = []
    for key, (W, H, top) in tops.items():
        lis = []
        for idx, (it, (x, y, w, h), lab) in enumerate(top):
            pos_ = f"left:{_pct(x, W)};top:{_pct(y, H)};width:{_pct(w, W)};height:{_pct(h, H)}"
            cls = f"c{idx % len(TINTS)} t-{lab['tier']}" + (" av" if lab["tier"] == "m" and lab.get("av") else "")
            av = ""
            if lab.get("av") and it.get("avatar"):
                a = lab["av"]
                av = (f'<img src="https://github.com/{esc(it["avatar"])}.png?size={a * 2}" width="{a}" height="{a}" '
                      f'alt="" loading="lazy" decoding="async">')
            stars = f'<span>{esc(it["stars_txt"])}</span>' if lab["tier"] in ("xl", "m") else ""
            lis.append(f'<li class="{cls}" style="{pos_}"><a href="{esc(it["href"])}" title="{esc(it["title"])}">{av}'
                       f'<b style="font-size:{lab["fs"]}px">{esc(lab["text"])}</b>{stars}</a></li>')
        out.append(f'<ol class="tmap tm tm-{key}" style="aspect-ratio:{W}/{H}">{"".join(lis)}</ol>')
    # tail: ranks kmin+1 .. cap_max, hidden per layout where band 0 has them or past the cap
    def hide(rank):
        c = []
        for key, (k, shown, _) in info.items():
            if rank <= k or rank > shown:
                c.append("tmx-" + key)
        return c
    bands, cur, lo_prev = [], None, None
    for r, it in enumerate(pos[kmin:cap_max], start=kmin + 1):
        lo, tier = next((lo, t) for lo, t in TAIL_BANDS if it["stars"] >= lo)
        if cur is None or cur["lo"] != lo:
            cur = {"lo": lo, "tier": tier, "tiles": [], "hide": None}
            bands.append(cur)
        cur["tiles"].append((r, it))
    for bi, b in enumerate(bands):
        hi = b["tiles"][0][1]["stars"]
        lis, hides = [], None
        for r, it in b["tiles"]:
            hc = hide(r)
            hides = set(hc) if hides is None else hides & set(hc)
            g = max(1.0, it["stars"] / max(1, b["tiles"][-1][1]["stars"]))
            av = (f'<img src="https://github.com/{esc(it["avatar"])}.png?size=64" width="32" height="32" alt="" loading="lazy" decoding="async">'
                  if b["tier"] == "m" and it.get("avatar") else "")
            stars = f'<span>{esc(it["stars_txt"])}</span>' if b["tier"] == "m" else ""
            cls = " ".join([f"c{(r + bi) % len(TINTS)}"] + hc)
            lis.append(f'<li class="{cls}" style="flex-grow:{g:.2f}"><a href="{esc(it["href"])}" title="{esc(it["title"])}">{av}'
                       f'<b>{esc(_tail_label(it, b["tier"]))}</b>{stars}</a></li>')
        hc = " ".join(sorted(hides or ()))
        lo_s = b["tiles"][-1][1]["stars"]
        out.append(f'<p class="tbh {hc}">&#9733; {_short(hi)}&ndash;{_short(lo_s)} &middot; tiles widen with stars</p>'
                   f'<ul class="tband t-{b["tier"]} {hc}">{"".join(lis)}</ul>')
    if any(v[2] > 0 for v in info.values()):
        spans = "".join(f'<b class="on-{k}">{v[2]:,} others</b><span class="on-{k}">Ranks {v[1] + 1:,}&ndash;{total:,}: open them in the table</span>'
                        for k, v in info.items())
        froms = " ".join(f'data-from-{k}="{v[1] + 1}"' for k, v in info.items())
        out.append(f'<a class="tmo" href="#table" data-mode="{mode}" {froms}>{spans}</a>')
    return f'<div class="tmbox">{"".join(out)}</div>{caption(info)}', info


STYLE = (
    "/* contributors treemap (treemap.py): squarified band 0 + flow tail, borderless */\n"
    ".tmwrap{container-type:inline-size;margin:var(--sp-4) 0}\n"
    ".tmwrap[hidden]{display:none}\n"
    ".tm{display:none}\n"
    ".tmap{position:relative;list-style:none;margin:0;padding:0;width:100%}\n"
    + "".join(f"@container {q}{{.tm-{k}{{display:block}}.tmx-{k}{{display:none!important}}.on-{k}{{display:block!important}}}}\n"
              for k, _, _, _, _, q in LAYOUTS)
    + f".tmap li{{position:absolute;overflow:hidden;border:0;padding:0 {GAP}px {GAP}px 0}}\n"
    + "".join(f".tmwrap .c{i} a{{background:{c}}}\n" for i, c in enumerate(TINTS))
    + ".tmap a,.tband a{display:flex;flex-direction:column;justify-content:center;gap:2px;width:100%;height:100%;"
    f"padding:{PAD_Y}px {PAD_X}px;text-decoration:none;color:{INK};line-height:{LINE};font-family:var(--font-data);transition:filter .12s}}\n"
    ".tmap a:hover,.tband a:hover,a.tmo:hover{filter:brightness(1.04) saturate(1.3)}\n"
    # keyboard focus keeps a visible ring (WCAG 2.4.7); it is not a tile border
    ".tmwrap a:focus-visible{outline:3px solid var(--z-focus);outline-offset:-3px}\n"
    ".tmap b,.tband b{display:block;font-weight:600;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;min-width:0}\n"
    f".tmap span,.tband span{{display:block;font-size:14px;color:{MUTE};white-space:nowrap;overflow:hidden;text-overflow:ellipsis;min-width:0}}\n"
    ".tmap .t-xl a{justify-content:flex-end}\n"
    ".tmwrap img{display:block;border-radius:50%;background:#fff;margin-bottom:auto}\n"
    f".tmap .av a,.tband.t-m a{{display:grid;grid-template-columns:{INLINE_AV}px minmax(0,1fr);column-gap:{INLINE_GAP}px;row-gap:2px;align-content:center}}\n"
    ".tmap .av img,.tband.t-m img{grid-row:1 / span 2;align-self:center;margin:0}\n"
    f".tband{{display:flex;flex-wrap:wrap;list-style:none;margin:0;padding:0;gap:0}}\n"
    ".tband::after{content:'';flex:999 1 0}\n"
    f".tband li{{flex-shrink:1;flex-basis:auto;min-width:0;max-width:100%;padding:0 {GAP}px {GAP}px 0}}\n"
    ".tband.t-m a{height:56px}.tband.t-m b{font-size:16px}\n"
    ".tband.t-s a{height:34px}.tband.t-s b{font-size:14px}\n"
    f".tband.t-xs a{{height:{XS_H}px;padding:{XS_PAD_Y}px {XS_PAD_X}px}}.tband.t-xs b{{font-size:{XS_FONT}px;font-weight:500}}\n"
    f".tbh{{font-family:var(--font-data);font-size:13px;color:{MUTE};margin:var(--sp-4) 0 var(--sp-2)}}\n"
    f"a.tmo{{display:flex;flex-direction:column;align-items:center;justify-content:center;gap:4px;height:{OTHERS_H}px;margin-top:var(--sp-3);"
    f"background:{OTHER_TINT};color:{INK};text-decoration:none;font-family:var(--font-data);text-align:center;padding:0 var(--sp-4)}}\n"
    ".tmo b,.tmo span,.tmcap span{display:none}.tmo b{font-size:22px;font-weight:700}"
    f".tmo span{{font-size:14px;color:{MUTE}}}\n"
    ".tmcap{font-size:var(--fs-13);color:var(--z-mute);margin:var(--sp-3) 0 0}\n"
)
