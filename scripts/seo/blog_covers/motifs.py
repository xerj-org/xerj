"""Square-matrix motifs for the blog cover series ("engineering field-log posters").

Every motif is a pure function ``motif(w, h, rng) -> str`` that returns SVG
markup drawn inside a ``w`` x ``h`` box whose origin is the top-left corner.
They draw with squares on a grid only (plus 1px hairlines and mono labels);
``dot`` is a filled cell, ``ring`` an empty one. Inks, on light paper:

    .y  yellow   #ffc400   fill only: XERJ / the one result the post is about
                           (its labels and hairlines print in ochre #a06800)
    .w  ink      #0b0b0d   structure, neutral series
    .b  blue     #6f8aa8   the comparison series, never XERJ
    .m  mute     #696762   annotation
    .f  rule     #cfcbbf   the empty cell, the background grid

Every number a motif draws or labels is quoted from the post it illustrates;
the comment above each motif names where. Randomness only ever decides
*placement* (jitter, scatter) and comes from ``rng``, a ``random.Random``
seeded with the post slug, so the output is identical on every run.
"""

from __future__ import annotations

import math
import random
from typing import Callable

# ── primitives ───────────────────────────────────────────────────────────────


def _f(v: float) -> str:
    return f"{v:.1f}".rstrip("0").rstrip(".")


def dot(x: float, y: float, r: float, ink: str) -> str:
    """One cell of the matrix: a square centred on (x, y). ``r`` is the
    half-pitch-scaled size the halftone asks for; the square keeps the same
    centre, so every screen stays on its grid."""
    if r <= 0.15:
        return ""
    s = r * 1.78
    return (f'<rect x="{_f(x - s / 2)}" y="{_f(y - s / 2)}" width="{_f(s)}" height="{_f(s)}" '
            f'class="{ink}"/>')


def ring(x: float, y: float, r: float, ink: str, sw: float = 1.2) -> str:
    """An empty cell: a square outline, the matrix's "not this one"."""
    s = r * 1.78
    return (f'<rect x="{_f(x - s / 2)}" y="{_f(y - s / 2)}" width="{_f(s)}" height="{_f(s)}" '
            f'class="{ink}-s" fill="none" stroke-width="{_f(sw)}"/>')


# Art-only renders (the in-post hero and the index thumbnail) set this to
# False: the page already carries the title, so those covers draw no text at
# all. The og card keeps its labels.
LABELS = True


def label(x: float, y: float, text: str, cls: str = "lab", anchor: str = "start") -> str:
    if not LABELS:
        return ""
    text = (text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;"))
    return f'<text x="{_f(x)}" y="{_f(y)}" class="{cls}" text-anchor="{anchor}">{text}</text>'


def hair(x1: float, y1: float, x2: float, y2: float, ink: str = "f", sw: float = 1) -> str:
    return (f'<line x1="{_f(x1)}" y1="{_f(y1)}" x2="{_f(x2)}" y2="{_f(y2)}" '
            f'class="{ink}-s" stroke-width="{_f(sw)}"/>')


def halftone(x: float, y: float, w: float, h: float, pitch: float,
             val: Callable[[float, float], float], ink: str,
             rng: random.Random | None = None, grain: float = 0.0) -> str:
    """A dot screen over a rectangle; ``val(u, v)`` in 0..1 sets dot area at the
    normalised position (u, v). ``grain`` drops dots at random, risograph-like."""
    out = []
    cols = max(1, int(w // pitch))
    rows = max(1, int(h // pitch))
    rmax = pitch * 0.5
    for j in range(rows):
        for i in range(cols):
            u = (i + 0.5) / cols
            v = (j + 0.5) / rows
            a = max(0.0, min(1.0, val(u, v)))
            if rng is not None and grain and rng.random() < grain:
                continue
            r = rmax * math.sqrt(a) * 0.96
            if rng is not None:
                r *= 1 + rng.uniform(-0.07, 0.07)
            out.append(dot(x + (i + 0.5) * pitch, y + (j + 0.5) * pitch, r, ink))
    return "".join(out)


def dotbar(x: float, y: float, length: float, rows: int, pitch: float, ink: str,
           fade: int = 5) -> str:
    """A horizontal bar made of a dot screen; the last ``fade`` columns thin out
    like an ink edge. ``length`` is honoured to the column."""
    cols = max(1, int(round(length / pitch)))
    out = []
    for i in range(cols):
        a = 1.0 if i < cols - fade else max(0.18, (cols - i) / (fade + 1))
        for j in range(rows):
            out.append(dot(x + (i + 0.5) * pitch, y + (j + 0.5) * pitch,
                           pitch * 0.5 * math.sqrt(a) * 0.96, ink))
    return "".join(out)


def dotpath(points: list[tuple[float, float]], step: float, r0: float, r1: float,
            ink: str) -> str:
    """Dots spaced ``step`` apart along a polyline, radius ramping r0 -> r1."""
    segs = list(zip(points, points[1:]))
    total = sum(math.dist(a, b) for a, b in segs) or 1.0
    out, walked, carry = [], 0.0, 0.0
    for a, b in segs:
        L = math.dist(a, b)
        t = carry
        while t <= L:
            k = (walked + t) / total
            px = a[0] + (b[0] - a[0]) * t / L
            py = a[1] + (b[1] - a[1]) * t / L
            out.append(dot(px, py, r0 + (r1 - r0) * k, ink))
            t += step
        carry = t - L
        walked += L
    return "".join(out)


def bezier(p0, p1, p2, n: int = 60) -> list[tuple[float, float]]:
    return [((1 - t) ** 2 * p0[0] + 2 * (1 - t) * t * p1[0] + t * t * p2[0],
             (1 - t) ** 2 * p0[1] + 2 * (1 - t) * t * p1[1] + t * t * p2[1])
            for t in (i / n for i in range(n + 1))]


def disc(cx: float, cy: float, R: float, pitch: float, ink: str,
         profile: Callable[[float], float] = lambda d: 1 - d * d) -> str:
    """A round halftone blob: dot area falls off with distance from the centre."""
    out = []
    n = int(R // pitch) + 1
    for j in range(-n, n + 1):
        for i in range(-n, n + 1):
            x, y = cx + i * pitch, cy + j * pitch
            d = math.hypot(x - cx, y - cy) / R
            if d > 1:
                continue
            out.append(dot(x, y, pitch * 0.5 * math.sqrt(max(0, profile(d))) * 0.96, ink))
    return "".join(out)


# ── motifs ───────────────────────────────────────────────────────────────────

def m_index28(w: float, h: float, rng: random.Random) -> str:
    """cutting-the-index-28-percent: the index as a block of squares, before and
    after. Areas are to scale: 5.52 MB before, 3.99 MB after (-28%), the whole-
    index figures in the post's lede and index card. The cut-away (the 28%)
    is the yellow L. The two side bars are the post's .seg (1,671 -> 449 kB,
    -73.1%) and .meta (200.8 -> 13.8 kB, -93.1%) rows, each against its own
    'before' length."""
    out = []
    S = min(h * 0.80, w * 0.58)
    pitch = max(8.0, S / 26)
    n = int(S // pitch)
    S = n * pitch
    x0, y0 = 0.0, 26.0
    k = int(round(n * math.sqrt(3.99 / 5.52)))   # after-block side, in cells
    for j in range(n):
        for i in range(n):
            cx, cy = x0 + (i + 0.5) * pitch, y0 + (j + 0.5) * pitch
            if i < k and j >= n - k:
                u, v = i / k, (j - (n - k)) / k
                out.append(dot(cx, cy, pitch * 0.40, "w"))
            else:
                out.append(dot(cx, cy, pitch * 0.40, "y"))
    out.append(label(x0, y0 - 12, "BEFORE · 5.52 MB", "lab"))
    out.append(label(x0, y0 + S + 22, "AFTER · 3.99 MB", "lab w"))
    bx = x0 + S + 30
    bw = w - bx
    out.append(label(bx, y0 + 40, "−28%", "big w"))
    if bw > 110:
        rows = [(".SEG", 1671, 449, "−73.1%"), (".META", 200.8, 13.8, "−93.1%")]
        by = y0 + S * 0.38
        bp = max(5.0, pitch * 0.55)
        for name, before, after, delta in rows:
            out.append(label(bx, by, f"{name} {delta}", "lab"))
            out.append(dotbar(bx, by + 10, bw, 2, bp, "f", 3))
            out.append(dotbar(bx, by + 14 + 2 * bp, max(bp, bw * after / before), 2, bp, "w", 1))
            by += S * 0.30
    return "".join(out)


def m_jevbars(w: float, h: float, rng: random.Random) -> str:
    """does-xerj-beat-jev: the verdict table as square-bars. nDCG@10 from the post:
    SciFact 0.6993 (hybrid) vs 0.7410 (BM25+Jev), NFCorpus 0.3448 vs 0.3312,
    FiQA 0.2382 (BM25) vs 0.3638 (BM25+Jev). One denominator, 0.7410, for every
    row. Last row: the FiQA rerank bill, $0.6375 vs $0 local."""
    out = []
    rows = [("SCIFACT", 0.6993, "hybrid", 0.7410, "BM25+Jev"),
            ("NFCORPUS", 0.3448, "hybrid", 0.3312, "BM25+Jev"),
            ("FIQA", 0.2382, "BM25", 0.3638, "BM25+Jev")]
    lab_w = 0.0
    full = w * 0.70
    gap = h / 4.6
    pitch = max(6.0, gap / 9.5)
    y = 8.0
    for name, a, an, b, bn in rows:
        out.append(label(0, y + 12, name, "lab"))
        out.append(dotbar(lab_w, y + 22, full * a / 0.7410, 3, pitch, "w"))
        out.append(label(lab_w + full * a / 0.7410 + 10, y + 22 + 2.2 * pitch, f"{a:.4f} {an}", "num w"))
        out.append(dotbar(lab_w, y + 26 + 3 * pitch, full * b / 0.7410, 3, pitch, "b"))
        out.append(label(lab_w + full * b / 0.7410 + 10, y + 26 + 5.2 * pitch, f"{b:.4f} {bn}", "num b"))
        y += gap
    out.append(hair(0, y - 4, w, y - 4, "f"))
    out.append(label(0, y + 16, "FIQA RERANK BILL", "lab"))
    out.append(dotbar(lab_w, y + 26, full, 3, pitch, "b"))
    out.append(label(lab_w + full + 10, y + 26 + 2.2 * pitch, "$0.6375", "num b"))
    out.append(f'<rect x="{_f(lab_w)}" y="{_f(y + 30 + 3.4 * pitch)}" width="{_f(2.2 * pitch)}" height="{_f(2.2 * pitch)}" class="yo"/>')
    out.append(label(lab_w + 2.2 * pitch + 10, y + 30 + 5.0 * pitch, "$0 · XERJ local", "num y"))
    return "".join(out)


def m_vote(w: float, h: float, rng: random.Random) -> str:
    """jev-vs-a-bm25-vote: Banking77, 500 decisions per arm as a 25x20
    square grid; filled = correct, outline = wrong (yellow: the vote's 45
    correct answers beyond Jev's 412), at the post's accuracies 0.9140 (BM25
    vote) and 0.8240 (Jev). The footer line is the SMS row: 0.9840 vs 0.9880,
    McNemar p = 0.75."""
    out = []
    cols, rows_n = 25, 20
    gw = (w - 36) / 2
    pitch = min(gw / cols, (h - 92) / rows_n)
    gy = 34.0
    jev_right = round(0.8240 * 500)
    for k, (name, acc, ink) in enumerate([("BM25 VOTE", 0.9140, "w"), ("JEV", 0.8240, "b")]):
        gx = k * (gw + 36)
        right = round(acc * 500)
        out.append(label(gx, gy - 14, f"{name} · {acc:.4f}", f"lab {ink}"))
        for n in range(500):
            i, j = n % cols, n // cols
            cx, cy = gx + (i + 0.5) * pitch, gy + (j + 0.5) * pitch
            if n < right:
                # yellow: the decisions the vote gets right that Jev does not
                out.append(dot(cx, cy, pitch * 0.40, "y" if (k == 0 and n >= jev_right) else ink))
            else:
                out.append(ring(cx, cy, pitch * 0.30, "m", 1.0))
    yb = gy + rows_n * pitch + 26
    out.append(label(0, yb, "BANKING77 · 77 CLASSES · 500 DECISIONS EACH", "lab"))
    out.append(label(0, yb + 22, "SMS SPAM 0.9840 vs 0.9880 · McNEMAR p = 0.75 · A TIE", "lab m"))
    return "".join(out)


def m_names(w: float, h: float, rng: random.Random) -> str:
    """one-vulnerability-three-names: RUSTSEC-2021-0003, GHSA-43w2-9j62-hq99 and
    CVE-2021-25900 (the post's lede) converging on one identity-resolved
    record; caption 4,107 filings -> 1,950 records, the post's own count."""
    out = []
    names = ["RUSTSEC-2021-0003", "GHSA-43w2-9j62-hq99", "CVE-2021-25900"]
    R = min(h * 0.27, w * 0.2)
    cx, cy = w - R - 6, h * 0.46
    out.append(disc(cx, cy, R, max(7.0, R / 9), "w"))
    c = max(7.0, R / 9) * 1.6
    out.append(f'<rect x="{_f(cx - c)}" y="{_f(cy - c)}" width="{_f(2 * c)}" height="{_f(2 * c)}" class="yo"/>')
    out.append(ring(cx, cy, R + 12, "y", 1.0))
    ys = [h * 0.12, h * 0.46, h * 0.80]
    for n, y in zip(names, ys):
        out.append(f'<rect x="0" y="{_f(y - 17)}" width="{_f(w * 0.40)}" height="30" class="w-s" fill="none" stroke-width="1"/>')
        out.append(label(10, y + 3, n, "num w"))
        p0 = (w * 0.40 + 6, y - 2)
        p2 = (cx - R - 16, cy)
        p1 = ((p0[0] + p2[0]) / 2, y)
        out.append(dotpath(bezier(p0, p1, p2), 9, 1.2, 3.2, "w"))
    out.append(label(cx, cy + R + 40, "1 RECORD · ed25519-SIGNED", "lab y", "middle"))
    out.append(label(0, h - 2, "4,107 FILINGS → 1,950 RECORDS", "lab"))
    return "".join(out)


def _gate(x: float, y: float, gw: float, gh: float, pitch: float, post_ink: str,
          glyph: str, glyph_ink: str) -> str:
    out = []
    cols = 2
    for side in (0, 1):
        px = x + (0 if side == 0 else gw - cols * pitch)
        ink = post_ink if not (post_ink == "split") else ("y" if side == 0 else "w")
        out.append(halftone(px, y, cols * pitch, gh, pitch, lambda u, v: 1.0, ink))
    lint_ink = "y" if post_ink == "split" else post_ink
    out.append(halftone(x, y - pitch * 1.4, gw, pitch, pitch, lambda u, v: 1.0, lint_ink))
    mx, my = x + gw / 2, y + gh * 0.45
    s = gw * 0.20
    if glyph == "check":
        out.append(dotpath([(mx - s, my), (mx - s * 0.25, my + s * 0.8), (mx + s * 1.1, my - s)], pitch * 0.8, 2.6, 2.6, glyph_ink))
    elif glyph == "cross":
        out.append(dotpath([(mx - s, my - s), (mx + s, my + s)], pitch * 0.8, 2.6, 2.6, glyph_ink))
        out.append(dotpath([(mx + s, my - s), (mx - s, my + s)], pitch * 0.8, 2.6, 2.6, glyph_ink))
    elif glyph == "half":
        out.append(dotpath([(mx - s, my), (mx - s * 0.25, my + s * 0.8), (mx + s * 0.2, my + s * 0.2)], pitch * 0.8, 2.6, 2.6, "y"))
        out.append(dotpath([(mx + s * 0.35, my - s), (mx + s * 1.1, my - s * 0.2)], pitch * 0.8, 2.6, 2.6, "w"))
    elif glyph == "dash":
        out.append(dotpath([(mx - s * 0.7, my), (mx + s * 0.7, my)], pitch * 0.8, 2.2, 2.2, glyph_ink))
    return "".join(out)


def m_gates(w: float, h: float, rng: random.Random) -> str:
    """the-rc80-gates: the program table, one gate per feature, in the post's
    order of verdicts: 3 PASS (xerj_map, /_ask, p_cal), 3 FAIL (decide head,
    judge, flywheel), 1 SPLIT (stemming), 1 REPORTED (the open model card), and
    the two rows the post marks unmeasured on purpose (max_tokens, _watcher)."""
    gates = [("xerj_map", "PASS"), ("/_ask", "PASS"), ("p_cal", "PASS"),
             ("decide", "FAIL"), ("judge", "FAIL"), ("flywheel", "FAIL"),
             ("stemming", "SPLIT"), ("model card", "REPORTED"),
             ("max_tokens", "NO GATE"), ("_watcher", "NOT RUN")]
    style = {"PASS": ("y", "check", "y"), "FAIL": ("w", "cross", "w"),
             "SPLIT": ("split", "half", "y"), "REPORTED": ("m", "dash", "m"),
             "NO GATE": ("f", "dash", "f"), "NOT RUN": ("f", "dash", "f")}
    out = []
    per_row = 5
    cw = w / per_row
    gw = cw * 0.62
    rh = (h - 40) / 2
    gh = rh * 0.52
    pitch = max(5.0, gw / 11)
    for n, (feat, verdict) in enumerate(gates):
        r, c = divmod(n, per_row)
        x = c * cw + (cw - gw) / 2 - cw * 0.06
        y = r * rh + rh * 0.18 + 12
        post, glyph, gink = style[verdict]
        out.append(_gate(x, y, gw, gh, pitch, post, glyph, gink))
        vcls = {"PASS": "lab y", "FAIL": "lab w", "SPLIT": "lab y"}.get(verdict, "lab m")
        out.append(label(x, y + gh + 22, verdict, vcls))
        out.append(label(x, y + gh + 40, feat, "num m"))
    out.append(label(0, h - 2, "10 FEATURES · 8 GATES · 3 PASS · 3 FAIL · 1 SPLIT", "lab"))
    return "".join(out)


def m_tiles(w: float, h: float, rng: random.Random) -> str:
    """the-corpus-hub: the registry as a 10x10 wall of pack tiles, 100 being
    the live-corpus count in the post's 2026-10-06 update. The four launch
    corpora are solid yellow; the one signed daily pack carries a seal frame."""
    out = []
    n = 10
    side = min((h - 44) / n, w * 0.78 / n)
    gap = side * 0.16
    t = side - gap
    sub = 4
    pitch = t / sub
    x0 = 0.0
    y0 = 6.0
    for k in range(100):
        i, j = k % n, k // n
        x, y = x0 + i * side, y0 + j * side
        if k < 4:
            out.append(halftone(x, y, t, t, pitch, lambda u, v: 1.0, "y"))
        else:
            d = 0.10 + 0.55 * rng.random()
            out.append(halftone(x, y, t, t, pitch, lambda u, v, d=d: d, "w"))
    sx, sy = x0 + 4 * side + t / 2, y0 + t / 2
    out.append(ring(sx, sy, t * 0.62, "y", 2))
    lx = x0 + n * side + 18
    if lx < w - 80:
        out.append(label(lx, y0 + 14, "4 AT LAUNCH", "lab y"))
        out.append(label(lx, y0 + 34, "+1 SIGNED PACK", "lab y"))
        out.append(label(lx, y0 + n * side - 26, "100 LIVE", "lab"))
        out.append(label(lx, y0 + n * side - 6, "2026-10-06", "lab m"))
    out.append(label(0, h - 2, "PINNED · LICENCED · CHECKSUMMED · SIGNED", "lab"))
    return "".join(out)


def m_network(w: float, h: float, rng: random.Random) -> str:
    """the-exploit-hub: a field of PoC documents and the three fresh CVEs of the
    eval. CVE-2026-23744 links to 36 PoC nodes (the post's 36 in-corpus PoC
    repositories); CVE-2026-105844 had no PoC anywhere, so one link to a
    neighbouring 2026 exploit; CVE-2026-97332 is the empty stratum, unlinked."""
    out = []
    H = h - 30
    pts = []
    tries = 0
    while len(pts) < 150 and tries < 6000:
        tries += 1
        p = (rng.uniform(8, w - 8), rng.uniform(8, H - 8))
        if all(math.dist(p, q) > 22 for q in pts):
            pts.append(p)
    for p in pts:
        near = sorted(pts, key=lambda q: math.dist(p, q))[1:3]
        for q in near:
            out.append(hair(p[0], p[1], q[0], q[1], "f", 0.8))
    A = (w * 0.30, H * 0.46)
    B = (w * 0.78, H * 0.22)
    C = (w * 0.78, H * 0.78)
    near_a = sorted(pts, key=lambda q: math.dist(A, q))[:36]
    for q in near_a:
        out.append(hair(A[0], A[1], q[0], q[1], "y", 0.9))
    for p in pts:
        hot = p in near_a
        out.append(dot(p[0], p[1], 3.2 if hot else 2.2, "y" if hot else "m"))
    nb = min((q for q in pts if math.dist(q, B) > 60), key=lambda q: math.dist(q, B))
    out.append(dotpath([B, nb], 8, 2.0, 2.0, "w"))
    out.append(dot(nb[0], nb[1], 4.5, "w"))
    out.append(disc(A[0], A[1], 30, 6.5, "y"))
    out.append(disc(B[0], B[1], 20, 6, "w"))
    out.append(f'<circle cx="{_f(C[0])}" cy="{_f(C[1])}" r="40" class="bgfill"/>')
    out.append(ring(C[0], C[1], 18, "w", 1.6))
    out.append(label(A[0], A[1] + 52, "CVE-2026-23744 · 36 POCS", "tag y", "middle"))
    out.append(label(B[0], B[1] + 40, "CVE-2026-105844 · A NEIGHBOUR", "tag w", "middle"))
    out.append(label(C[0], C[1] + 38, "CVE-2026-97332 · ADDS ~0", "tag m", "middle"))
    out.append(label(0, h - 2, "1.09M POC DOCUMENTS · 3 FRESH CVES · LOSSES COUNTED", "lab"))
    return "".join(out)


def m_scars(w: float, h: float, rng: random.Random) -> str:
    """eval-scars-from-agent-memory: six scratch marks, one per pattern the
    guest post credits, each tagged with that pattern's own figure from the
    post: topic slicing at recall@1, the +2.75pt incumbent, group-by-qid, the
    0.87 classifier vs the 0.78 always-yes baseline, the ~4x unpaired
    overstatement, and the mask bug that scores every query zero."""
    tags = ["TOPIC SLICING · r@1", "+2.75pt INCUMBENT", "GROUP BY QID",
            "0.87 vs 0.78 ALWAYS-YES", "4× UNPAIRED", "MASK → 0 HITS"]
    out = []
    H = h - 30
    # faint scatter of nicks across the whole field
    for _ in range(90):
        x, y = rng.uniform(0, w), rng.uniform(0, H)
        a = rng.uniform(-1.2, -0.6)
        L = rng.uniform(6, 16)
        out.append(dotpath([(x, y), (x + L * math.cos(a), y + L * math.sin(a))], 4, 1.0, 1.0, "f"))
    cw, rh = w / 3, H / 2
    for k, tag in enumerate(tags):
        r, c = divmod(k, 3)
        cx = c * cw + cw * rng.uniform(0.30, 0.55)
        cy = r * rh + rh * rng.uniform(0.32, 0.50)
        ink = "y" if k == 5 else "w"
        a = rng.uniform(-1.25, -0.85)
        strokes = 3 + (k % 3)
        L = rh * 0.52
        for s in range(strokes):
            ox = cx + (s - strokes / 2) * 11 + rng.uniform(-2, 2)
            oy = cy + rng.uniform(-4, 4)
            p0 = (ox - L / 2 * math.cos(a), oy - L / 2 * math.sin(a))
            p1 = (ox + L / 2 * math.cos(a), oy + L / 2 * math.sin(a))
            out.append(dotpath([p0, p1], 5.2, 1.4, 3.0, ink))
        out.append(label(c * cw + 4, r * rh + rh * 0.92, f"{k + 1:02d}  {tag}", f"tag {ink}"))
    out.append(label(0, h - 2, "SIX PATTERNS · EACH COST A REAL FAILURE", "lab"))
    return "".join(out)


def m_route(w: float, h: float, rng: random.Random) -> str:
    """corpus-hub-v2: the router as a wall of pack tiles. Back wall: 251 faint
    tiles, the post's count of distinct slugs with a signed v2 pack on GitHub
    Releases. Front: the 14 corpora in the signed catalogue the router reads
    (generated 2026-10-18), by slug. The question from the post's lede routes
    to lz4, whose 4.4 MB pack is pulled to a local node on port 9300. Timings
    are the post's table: 1.72-2.27 s cold, 37-57 ms warm."""
    slugs = ["act", "age", "agentops", "aiohttp", "alertmanager", "arkworks-algebra",
             "arroyo", "astro", "attack-mappings-explorer", "autorag", "avro",
             "lz4", "snappy", "zstd"]
    out = []
    # back wall: 251 packs on Releases (re-counted 2026-10-20); 36 x 7 holds them
    cols = 36
    bp = w / cols
    for k in range(251):
        i, j = k % cols, k // cols
        out.append(halftone(i * bp + 1, 30 + j * bp + 1, bp - 3, bp - 3, (bp - 3) / 2,
                            lambda u, v: 0.5, "f"))
    wall_h = 30 + 7 * bp
    out.append(label(0, 18, "xerj \"how does lz4 decompress a block\"", "num w"))
    # front: the 14 catalogue tiles
    tc = 7
    tw = (w - 6 * 10) / tc
    ty0 = wall_h + 22
    # two tile rows, then the node and the caption must fit under them
    th = min(tw * 0.62, (h - 40 - ty0 - 34) / 2 - 30)
    lz = None
    for k, slug in enumerate(slugs):
        i, j = k % tc, k // tc
        x, y = i * (tw + 10), ty0 + j * (th + 30)
        hot = slug == "lz4"
        out.append(halftone(x, y, tw, th, tw / 9, (lambda u, v: 0.35 + 0.65 * (1 - v)) if hot
                            else (lambda u, v: 0.16), "y" if hot else "w", rng))
        name = slug if len(slug) <= 9 else slug[:8] + "…"
        out.append(label(x, y + th + 15, name, "tag y" if hot else "tag m"))
        if hot:
            lz = (x + tw / 2, y + th / 2)
    # question -> lz4 -> node
    gx = 4 * (tw + 10) - 5   # the gutter left of lz4's column
    out.append(dotpath([(8, 26), (8, ty0 - 11), (gx, ty0 - 11), (gx, lz[1]),
                        (lz[0] - tw / 2 - 3, lz[1])], 7, 1.6, 2.8, "y"))
    ny = ty0 + 2 * (th + 30) + 22
    out.append(dotpath([(lz[0], lz[1] + th / 2 + 22), (lz[0], ny - 14)], 7, 2.6, 2.6, "y"))
    out.append(ring(lz[0], ny, 12, "y", 2))
    out.append(dot(lz[0], ny, 5, "y"))
    out.append(label(lz[0] - 22, ny + 5, "4.4 MB PACK → LOCAL NODE :9300", "lab y", "end"))
    out.append(label(0, h - 2, "14 IN THE SIGNED CATALOGUE · 1.72–2.27 s COLD · 37–57 ms WARM", "lab"))
    return "".join(out)


def m_budget(w: float, h: float, rng: random.Random) -> str:
    """rc99-smart-autoindex-harness-tools.

    Eight squares, one per GiB of the auto tier below 64 GiB
    (``tiered_cap_mb`` in engine/crates/xerj-engine/src/governor.rs). The
    first two are yellow: the 2 GiB memtable budget on that 8 GiB cap
    (``auto_memtable_budget``, 25% floored at 2 GiB). Eight small squares
    are the 8 MiB document ceiling (``MAX_INDEXED_DOCUMENT_BYTES``). One
    outlined square, set apart, is uncompressed JSON streamed past the
    64 MiB whole-file cap. rng is unused; the layout is fixed.
    """
    del rng
    cols = 8
    gap = max(8.0, min(18.0, w * 0.018))
    caption = 20.0 if LABELS else 0.0
    gap_rows = 22.0 if LABELS else 14.0
    top = 8.0
    avail = h - top - (caption * 2) - gap_rows
    s = min((w - gap * (cols - 1)) / cols, avail / 1.42)
    s = max(s, 12.0)
    small = min(s * 0.36, 32.0)
    sg = max(4.0, small * 0.45)
    left_w = cols * small + (cols - 1) * sg
    sep = max(small * 1.45, 18.0)
    row_w = left_w + sep + small
    if row_w > w - 4:
        scale = (w - 4) / row_w
        small *= scale
        sg *= scale
        sep *= scale
        left_w = cols * small + (cols - 1) * sg
        row_w = left_w + sep + small
    grid_w = cols * s + (cols - 1) * gap
    block = s + caption + gap_rows + small + caption
    y0 = top + max(0.0, (h - top - block) / 2)
    x0 = (w - grid_w) / 2
    out: list[str] = []
    for i in range(cols):
        x = x0 + i * (s + gap) + s / 2
        out.append(dot(x, y0 + s / 2, s / 1.78, "y" if i < 2 else "w"))
    cap_y = y0 + s + (16 if LABELS else 0)
    out.append(label(x0, cap_y, "8 GiB TIER", "num w"))
    out.append(label(x0 + grid_w, cap_y, "2 GiB MEMTABLE", "num y", "end"))
    y1 = cap_y + gap_rows
    lx = (w - row_w) / 2
    for i in range(cols):
        x = lx + i * (small + sg) + small / 2
        out.append(dot(x, y1 + small / 2, small / 1.78, "w"))
    boundary = lx + left_w + sep / 2
    out.append(hair(boundary, y1 - 3, boundary, y1 + small + 3, "y", 1.4))
    yx = lx + left_w + sep + small / 2
    out.append(ring(yx, y1 + small / 2, small / 1.78, "y", 1.6))
    lab_y = y1 + small + (16 if LABELS else 0)
    out.append(label(lx, lab_y, "8 MiB DOCUMENT", "lab w"))
    out.append(label(yx + small / 2, lab_y, "PAST 64 MiB", "lab y", "end"))
    return "".join(out)


MOTIFS: dict[str, Callable[[float, float, random.Random], str]] = {
    "index28": m_index28,
    "jevbars": m_jevbars,
    "vote": m_vote,
    "names": m_names,
    "gates": m_gates,
    "tiles": m_tiles,
    "network": m_network,
    "scars": m_scars,
    "route": m_route,
    "budget": m_budget,
}
