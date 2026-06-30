#!/usr/bin/env python3
"""
XERJ.ai — Brandbook PDF generator.

Produces a single self-contained HTML file structured as a real
brand book (cover · colophon · contents · chapter dividers · chapters
· back cover), which puppeteer then prints to PDF.

This is NOT a direct dump of the website. Charts and logos are
regenerated here at print-tuned sizes, and every chapter starts on a
fresh page via CSS page-break-before.
"""
import math
import random
from pathlib import Path

random.seed(42)

# ============================================================
# CHART PRIMITIVES — all output inline SVG. 1px strokes only.
# ============================================================

def esc(s):
    return str(s).replace('&', '&amp;').replace('<', '&lt;').replace('>', '&gt;')

def fmt_num(v):
    if abs(v) >= 1e6: return f"{v/1e6:.2f}M"
    if abs(v) >= 1e3: return f"{v/1e3:.1f}K"
    if v == int(v):   return f"{int(v):,}"
    return f"{v:.1f}"

def build_points(values, w, h):
    mn, mx = min(values), max(values)
    rng = (mx - mn) or 1
    n = len(values)
    step = w / (n - 1) if n > 1 else w
    return " ".join(
        f"{i*step:.1f},{h - ((v-mn)/rng)*h:.1f}" for i, v in enumerate(values)
    ), mn, mx

# --- deterministic demo data ---
series_latency   = [24 + 6 * math.sin(i / 4.2) + 3 * math.cos(i / 2.1) + ((i * 7) % 5) * 0.4 for i in range(48)]
spark_throughput = [1.30 + 0.12 * math.sin(i / 3) + 0.04 * math.cos(i / 1.5) + ((i * 11) % 3) * 0.02 for i in range(48)]

topn_items = [
    ("/v2/search",      4230),
    ("/v2/catalog",     3180),
    ("/v2/cart",        2564),
    ("/auth/session",   1923),
    ("/billing/create", 1340),
    ("/users/me",        912),
    ("/admin/logs",      680),
    ("/static/favicon",  445),
]

dist_segments = [("2XX", 9620), ("3XX", 410), ("4XX", 190), ("5XX", 48)]

heat_rows_labels = ['COHORT A','COHORT B','COHORT C','COHORT D','COHORT E','COHORT F','COHORT G']
heat_cols = ['00','02','04','06','08','10','12','14','16','18','20','22']
heat_matrix = [
    [round(80 + 60 * math.cos(((c * 2 - 14) / 24) * 2 * math.pi) + r * 5 + ((r + c * 7) % 9))
     for c in range(12)]
    for r in range(7)
]

# --------- CHART SVG BUILDERS ---------

def spark_svg(values, w=260, h=44):
    pts, _, _ = build_points(values, w, h)
    return f'<svg viewBox="0 0 {w} {h}" preserveAspectRatio="none" style="width:100%; height:{h}px;"><polyline points="{pts}" fill="none" stroke="currentColor" stroke-width="1"/></svg>'

def series_svg(values, w=1200, h=160):
    pts, mn, mx = build_points(values, w, h)
    return f'<svg viewBox="0 0 {w} {h}" preserveAspectRatio="none" style="width:100%; height:{h}px;"><polyline points="{pts}" fill="none" stroke="currentColor" stroke-width="1"/></svg>', mn, mx

def topn_rows_html(items):
    total = sum(v for _, v in items) or 1
    mx = max(v for _, v in items) or 1
    rows = []
    for label, value in items:
        pct = 100 * value / total
        bw = (value / mx) * 1000
        rows.append(f'''    <div class="row">
      <span class="row__label mono">{label}</span>
      <span class="row__val mono">{value:,}</span>
      <span class="row__bar"><svg viewBox="0 0 1000 6" preserveAspectRatio="none"><line x1="0" y1="3" x2="{bw:.1f}" y2="3" stroke="currentColor" stroke-width="1"/></svg></span>
      <span class="row__pct mono">{pct:.1f}%</span>
    </div>''')
    return "\n".join(rows)

def dist_svg_html(segments):
    total = sum(v for _, v in segments) or 1
    lines = []
    legend = []
    x = 0.0
    for i, (lbl, v) in enumerate(segments):
        segw = (v / total) * 1200
        op = max(0.12, 1 - i * 0.2)
        if segw > 0.5:
            lines.append(f'<line x1="{x:.1f}" y1="5" x2="{(x+segw):.1f}" y2="5" stroke="currentColor" stroke-width="1" stroke-opacity="{op:.2f}"/>')
        x += segw
        pct = 100 * v / total
        legend.append(f'''      <div class="stack"><span class="key">{lbl}</span><span class="mono big">{v:,}</span><span class="mono faint">{pct:.1f}%</span></div>''')
    return (
        f'<svg class="chart dist-bar" viewBox="0 0 1200 10" preserveAspectRatio="none">{"".join(lines)}</svg>',
        "\n".join(legend)
    )

def heatmap_html(rows_labels, cols, matrix):
    flat = [v for row in matrix for v in row]
    mn, mx = min(flat), max(flat)
    rng = (mx - mn) or 1
    header = '<div class="heatmap-row heatmap-head"><span class="row-label">&nbsp;</span>' + "".join(
        f'<span class="hc">{c}</span>' for c in cols) + '</div>'
    body = []
    for i, row in enumerate(matrix):
        cells = "".join(
            f'<span class="hc" style="opacity:{(0.22 + 0.78 * (v - mn) / rng):.2f};">{v}</span>'
            for v in row
        )
        body.append(f'<div class="heatmap-row"><span class="row-label">{rows_labels[i]}</span>{cells}</div>')
    return header + "".join(body)

def gauge_html(value, mn=0, mx=100, threshold=85, label="DISK PRESSURE · WAL"):
    frac = (value - mn) / (mx - mn or 1)
    x = frac * 600
    tx = ((threshold - mn) / (mx - mn or 1)) * 600
    return f'''
    <div style="display:flex; align-items:baseline; justify-content:space-between;">
      <div style="font-family: var(--font-display); font-weight:900; font-size:72px; color:var(--z-accent); line-height:1;">{value}<span style="font-family:var(--font-data); font-size:20px; color:var(--z-mute); letter-spacing:var(--track-ui); margin-left:6px;">%</span></div>
      <span class="kicker">THRESHOLD · {threshold}%</span>
    </div>
    <svg viewBox="0 0 600 14" preserveAspectRatio="none" style="width:100%; height:14px; margin-top:10px;">
      <line x1="0" y1="13" x2="600" y2="13" stroke="currentColor" stroke-width="1" stroke-opacity="0.2"/>
      <line x1="0" y1="13" x2="{x:.1f}" y2="13" stroke="currentColor" stroke-width="1"/>
      <line x1="{tx:.1f}" y1="3" x2="{tx:.1f}" y2="13" stroke="currentColor" stroke-width="1" stroke-opacity="0.5"/>
    </svg>
    <div style="display:flex; justify-content:space-between; margin-top:4px; font-family:var(--font-data); font-size:10px; letter-spacing:var(--track-ui); color:var(--z-mute);">
      <span>0</span><span>100 %</span>
    </div>'''

def multiples_html():
    items = []
    for i in range(8):
        label = ['API GATEWAY','AUTH','BILLING','CHECKOUT','SEARCH','CATALOG','INGEST','VECTORS'][i]
        phase = i * 0.6
        vals = [50 + 10*math.sin(j/3 + phase) + 4*math.cos(j/1.5) + ((i+j)%3)*0.8 for j in range(24)]
        val = round(vals[-1])
        pts, _, _ = build_points(vals, 180, 22)
        items.append((label, val, pts))
    cells = []
    for label, val, pts in items:
        cells.append(f'''      <div class="mult-cell">
        <div class="mult-head"><span>{label}</span><span class="mono" style="color:var(--z-ink); font-weight:700;">{val}</span></div>
        <svg viewBox="0 0 180 22" preserveAspectRatio="none"><polyline points="{pts}" fill="none" stroke="currentColor" stroke-width="1"/></svg>
      </div>''')
    return '<div class="mult-grid">' + "\n".join(cells) + '</div>'

def bar_html():
    vals = [50 + 22*math.sin(i/5.5) + 9*math.cos(i/3.2) + ((i*37)%7) for i in range(24)]
    mx = max(vals); mn = min(vals)
    w = 1200; h = 150
    step = w / 24
    lines = []
    for i, v in enumerate(vals):
        xp = (i + 0.5) * step
        yp = h - (v / mx) * h
        lines.append(f'<line x1="{xp:.1f}" y1="{h}" x2="{xp:.1f}" y2="{yp:.1f}" stroke="currentColor" stroke-width="1"/>')
    return f'''
    <svg viewBox="0 0 {w} {h}" preserveAspectRatio="none" style="width:100%; height:{h}px;">{"".join(lines)}</svg>
    <div class="legend">
      <span>00:00</span>
      <span class="mid">min <span class="mono">{mn:.0f}</span> · peak <span class="mono">{mx:.0f}</span></span>
      <span>23:00</span>
    </div>''', mn, mx

def scatter_html():
    pts = []
    for _ in range(80):
        t = random.random()
        x = 20 + t * 80 + (random.random() - 0.5) * 20
        y = 10 + t * 70 + (random.random() - 0.5) * 25
        pts.append((x, y))
    sx = [p[0] for p in pts]; sy = [p[1] for p in pts]
    xmn, xmx = min(sx), max(sx); ymn, ymx = min(sy), max(sy)
    xr = (xmx - xmn) or 1; yr = (ymx - ymn) or 1
    dots = "".join(
        f'<span style="position:absolute; left:{((x-xmn)/xr)*100:.2f}%; top:{((ymx-y)/yr)*100:.2f}%; transform:translate(-50%,-50%); font-family:var(--font-data); font-size:16px; color:var(--z-ink); line-height:1;">·</span>'
        for x, y in pts
    )
    return f'''
    <div style="position:relative; width:100%; height:220px;">{dots}</div>
    <div class="legend">
      <span>LATENCY <span class="mono">{xmn:.0f}..{xmx:.0f} ms</span></span>
      <span class="mid"><span class="mono">{len(pts)}</span> points</span>
      <span>THROUGHPUT <span class="mono">{ymn:.0f}..{ymx:.0f} rps</span></span>
    </div>'''

def stacked_html():
    base = [('2XX', 9620), ('3XX', 410), ('4XX', 190), ('5XX', 48)]
    regions = [('US', base),
               ('DE', [(l, int(v*0.6)) for l, v in base]),
               ('JP', [(l, int(v*0.45)) for l, v in base]),
               ('BR', [(l, int(v*0.3)) for l, v in base])]
    rows = []
    for region, segs in regions:
        total = sum(v for _, v in segs) or 1
        x = 0.0
        lines = []
        for i, (lbl, v) in enumerate(segs):
            segw = (v / total) * 1200
            op = max(0.14, 1 - i * 0.16)
            if segw > 0.5:
                lines.append(f'<line x1="{x:.1f}" y1="5" x2="{(x+segw):.1f}" y2="5" stroke="currentColor" stroke-width="1" stroke-opacity="{op:.2f}"/>')
            x += segw
        rows.append(f'''      <div class="stk-row">
        <span class="stk-lbl">{region}</span>
        <svg viewBox="0 0 1200 10" preserveAspectRatio="none" style="width:100%; height:10px;">{"".join(lines)}</svg>
        <span class="mono faint">{fmt_num(total)}</span>
      </div>''')
    return '<div class="stk-wrap">' + "\n".join(rows) + '</div>' + '<div class="stk-legend">2XX · 3XX · 4XX · 5XX</div>'

def treemap_html():
    tree = [
        ("root", 13000, [
            ("api-gateway", 5100, [
                ("/v2/search", 2400, []),
                ("/v2/cart",   1800, []),
                ("/v2/catalog", 900, []),
            ]),
            ("auth-service", 2400, []),
            ("billing",      1600, []),
            ("checkout",     1400, []),
        ]),
    ]
    def walk(items, depth, parent_total=None):
        total = parent_total or sum(v for _, v, _ in items) or 1
        mx = max(v for _, v, _ in items) if items else 1
        out = []
        for label, value, kids in items:
            frac = value / mx
            pct = 100 * value / total
            xw = 200 * frac
            indent = '·&nbsp;&nbsp;' * depth
            op = max(0.25, 1 - depth * 0.2)
            out.append(f'''    <div class="row">
      <span class="row__label mono"><span class="faint">{indent}</span>{label}</span>
      <span class="row__val mono">{fmt_num(value)}</span>
      <span class="row__bar"><svg viewBox="0 0 200 6" preserveAspectRatio="none"><line x1="0" y1="3" x2="{xw:.1f}" y2="3" stroke="currentColor" stroke-width="1" stroke-opacity="{op:.2f}"/></svg></span>
      <span class="row__pct mono">{pct:.1f}%</span>
    </div>''')
            if kids:
                out.append(walk(kids, depth + 1, value))
        return "\n".join(out)
    return '<div class="topn-list">' + walk(tree, 0) + '</div>'

def events_html():
    events = [
        ('23:14:02.081', 'err',  'upstream timed out while connecting to upstream · tenant=acme · req=a1f2…'),
        ('23:13:51.602', 'warn', 'slow query 842 ms · SELECT * FROM orders WHERE customer_id = ?'),
        ('23:13:04.114', 'info', 'batch 238 committed · 14,402 docs · 112 ms · lsn=0x3F A2 1C'),
        ('23:12:48.771', 'warn', 'rate limit approaching 85% for tenant=acme · 8,452/10,000 req/min'),
        ('23:12:09.033', 'info', 'segment merged · 3 → 1 · 412 MB → 386 MB · 640 ms'),
        ('23:11:52.118', 'info', 'ingest partition 4 caught up · lag 0 ms · commits/s 1,212'),
        ('23:11:41.884', 'err',  'chunk fetch 502 from storage-us-west-2 · retried once · succeeded'),
    ]
    def sev_color(sev):
        return {'err': 'var(--z-accent)', 'warn': 'var(--z-ink)', 'info': 'var(--z-mute)'}[sev]
    rows = []
    for at, sev, msg in events:
        rows.append(f'''    <div class="ev-row">
      <span class="ev-at">{at}</span>
      <span class="ev-sev" style="color:{sev_color(sev)};">{sev}</span>
      <span class="ev-msg">{esc(msg)}</span>
    </div>''')
    return '<div class="ev-list">' + "\n".join(rows) + '</div>'

def embedspace_html():
    clusters = [
        ('INVOICES',    1.2, 2.8, 22, 1.0),
        ('CONTRACTS',   3.4, 1.9, 18, 1.1),
        ('CHAT·SUPPORT',2.0, 0.6, 20, 0.9),
        ('CODE',        0.4, 1.2, 16, 0.7),
        ('POLICIES',    3.2, 3.6, 14, 0.8),
    ]
    embed = []
    for name, cx, cy, n, sp in clusters:
        pts = [(cx + (random.random()-0.5)*sp, cy + (random.random()-0.5)*sp) for _ in range(n)]
        embed.append((name, pts, (cx, cy)))

    def convex_hull(points):
        pts = sorted(points)
        if len(pts) < 3: return pts
        def cross(O, A, B): return (A[0]-O[0])*(B[1]-O[1]) - (A[1]-O[1])*(B[0]-O[0])
        lower = []
        for p in pts:
            while len(lower) >= 2 and cross(lower[-2], lower[-1], p) <= 0: lower.pop()
            lower.append(p)
        upper = []
        for p in reversed(pts):
            while len(upper) >= 2 and cross(upper[-2], upper[-1], p) <= 0: upper.pop()
            upper.append(p)
        return lower[:-1] + upper[:-1]

    W, H, pad = 1200, 360, 40
    ax = [p[0] for _, pts, _ in embed for p in pts]
    ay = [p[1] for _, pts, _ in embed for p in pts]
    xmn, xmx = min(ax), max(ax); ymn, ymx = min(ay), max(ay)
    px = lambda x: pad + ((x - xmn) / (xmx - xmn)) * (W - pad * 2)
    py = lambda y: pad + (1 - (y - ymn) / (ymx - ymn)) * (H - pad * 2)

    out = []
    for ci, (_, pts, _) in enumerate(embed):
        hull = convex_hull(pts)
        if len(hull) < 3: continue
        closed = hull + [hull[0]]
        poly = " ".join(f"{px(x):.1f},{py(y):.1f}" for x, y in closed)
        op = max(0.22, 0.55 - ci * 0.07)
        out.append(f'<polyline points="{poly}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="{op:.2f}"/>')
    for ci, (_, pts, _) in enumerate(embed):
        op = max(0.35, 1 - ci * 0.09)
        for x, y in pts:
            out.append(f'<text x="{px(x):.1f}" y="{py(y):.1f}" fill="currentColor" opacity="{op:.2f}" font-family="var(--font-data)" font-size="14" text-anchor="middle" dominant-baseline="middle">·</text>')
    for name, _, (cx, cy) in embed:
        out.append(f'<text x="{px(cx):.1f}" y="{py(cy)-14:.1f}" fill="currentColor" font-family="var(--font-prose)" font-size="11" font-weight="700" letter-spacing="1.5" text-anchor="middle">{name}</text>')
    q = (2.1, 2.2)
    out.append(f'<line x1="{px(q[0]):.1f}" y1="0" x2="{px(q[0]):.1f}" y2="{H}" stroke="var(--z-accent)" stroke-width="1" stroke-opacity="0.45"/>')
    out.append(f'<line x1="0" y1="{py(q[1]):.1f}" x2="{W}" y2="{py(q[1]):.1f}" stroke="var(--z-accent)" stroke-width="1" stroke-opacity="0.45"/>')
    out.append(f'<text x="{px(q[0]):.1f}" y="{py(q[1]):.1f}" fill="var(--z-accent)" font-family="var(--font-data)" font-size="18" text-anchor="middle" dominant-baseline="middle">×</text>')
    out.append(f'<text x="{px(q[0])+8:.1f}" y="{py(q[1])-8:.1f}" fill="var(--z-accent)" font-family="var(--font-data)" font-size="11" letter-spacing="0.08em">QUERY</text>')

    pts_count = sum(len(p) for _, p, _ in embed)
    return f'''
    <svg viewBox="0 0 {W} {H}" preserveAspectRatio="none" style="width:100%; height:{H}px;">
      {chr(10).join(out)}
    </svg>
    <div class="legend">
      <span class="mono faint">UMAP · {pts_count} embeddings · {len(embed)} clusters</span>
    </div>'''

def chordarcs_html():
    sources = [
        ('q1', 'q · "refund policy"'),
        ('q2', 'q · "price breakdown"'),
        ('q3', 'q · "SLA for enterprise"'),
        ('q4', 'q · "data retention"'),
        ('q5', 'q · "schema change"'),
    ]
    targets = [
        ('d1', 'doc · policy/refunds.md'),
        ('d2', 'doc · pricing/tiers.md'),
        ('d3', 'doc · contracts/sla.md'),
        ('d4', 'doc · security/retention.md'),
        ('d5', 'doc · runbooks/migrate.md'),
        ('d6', 'doc · faq/common.md'),
    ]
    flows = [
        ('q1','d1',0.95),('q1','d6',0.55),
        ('q2','d2',0.92),('q2','d6',0.35),
        ('q3','d3',0.88),('q3','d2',0.42),
        ('q4','d4',0.91),('q4','d1',0.30),
        ('q5','d5',0.82),('q5','d6',0.40),
    ]
    W, H, pad = 1200, 380, 24
    inner = H - pad * 2
    srcY = lambda i: pad + ((i + 0.5) / len(sources)) * inner
    tgtY = lambda i: pad + ((i + 0.5) / len(targets)) * inner
    leftX, rightX = 240, W - 240
    mxW = max(f[2] for f in flows)

    arcs = []
    for fr, to, w in flows:
        si = next(i for i, s in enumerate(sources) if s[0] == fr)
        ti = next(i for i, t in enumerate(targets) if t[0] == to)
        y1, y2 = srcY(si), tgtY(ti)
        cx1 = leftX + (rightX - leftX) * 0.38
        cx2 = leftX + (rightX - leftX) * 0.62
        op = max(0.1, min(0.9, (w / mxW) * 0.85))
        arcs.append(f'<path d="M {leftX},{y1:.1f} C {cx1:.1f},{y1:.1f} {cx2:.1f},{y2:.1f} {rightX},{y2:.1f}" fill="none" stroke="currentColor" stroke-width="1" stroke-opacity="{op:.2f}"/>')

    sl = "\n".join(
        f'<text x="{leftX - 12:.1f}" y="{srcY(i):.1f}" fill="currentColor" font-family="var(--font-data)" font-size="11" text-anchor="end" dominant-baseline="middle">{esc(s[1])}</text>'
        for i, s in enumerate(sources))
    tl = "\n".join(
        f'<text x="{rightX + 12:.1f}" y="{tgtY(i):.1f}" fill="currentColor" font-family="var(--font-data)" font-size="11" dominant-baseline="middle">{esc(t[1])}</text>'
        for i, t in enumerate(targets))
    hs = f'<text x="{leftX - 12:.1f}" y="{pad - 8:.1f}" fill="currentColor" opacity="0.6" font-family="var(--font-prose)" font-size="11" font-weight="600" letter-spacing="2" text-anchor="end">QUERY</text>'
    ht = f'<text x="{rightX + 12:.1f}" y="{pad - 8:.1f}" fill="currentColor" opacity="0.6" font-family="var(--font-prose)" font-size="11" font-weight="600" letter-spacing="2">CHUNK</text>'

    return f'''
    <svg viewBox="0 0 {W} {H}" preserveAspectRatio="none" style="width:100%; height:{H}px; overflow:visible;">
      {chr(10).join(arcs)}
      {sl}
      {tl}
      {hs}
      {ht}
    </svg>
    <div class="legend">
      <span class="mono faint">{len(flows)} flows · {len(sources)} → {len(targets)}</span>
    </div>'''

def flowband_html():
    segs = [('SYSTEM',1200),('CONTEXT',6400),('RETRIEVAL',14200),('HISTORY',2800),('ANSWER',3400)]
    total = sum(v for _, v in segs) or 1
    W, H = 1200, 16
    parts = []
    x = 0.0
    for i, (lbl, v) in enumerate(segs):
        w = (v / total) * W
        op = max(0.28, 1 - i * 0.16)
        parts.append(f'<line x1="{x:.1f}" y1="2" x2="{x:.1f}" y2="14" stroke="currentColor" stroke-width="1" stroke-opacity="0.4"/>')
        if w > 1:
            parts.append(f'<line x1="{x:.1f}" y1="8" x2="{x+w:.1f}" y2="8" stroke="currentColor" stroke-width="2" stroke-opacity="{op:.2f}"/>')
        x += w
    parts.append(f'<line x1="{W}" y1="2" x2="{W}" y2="14" stroke="currentColor" stroke-width="1" stroke-opacity="0.4"/>')
    labels = []
    for i, (lbl, v) in enumerate(segs):
        pct = 100 * v / total
        color = 'var(--z-accent)' if i == 2 else 'var(--z-mute)'
        labels.append(f'''      <div class="fb-lbl">
        <div class="fb-name" style="color:{color};">{lbl}</div>
        <div class="mono" style="color:var(--z-ink); font-size:13px;">{fmt_num(v)} tok</div>
        <div class="mono faint">{pct:.1f} %</div>
      </div>''')
    return f'''
    <svg viewBox="0 0 {W} {H}" preserveAspectRatio="none" style="width:100%; height:{H}px; overflow:visible; display:block;">{"".join(parts)}</svg>
    <div class="fb-grid">
{chr(10).join(labels)}
    </div>'''

def parallelcoords_html():
    dims = [('LATENCY',8,60),('TOKENS',200,4800),('COST $',0.002,0.12),('SCORE',0.4,0.98),('RETRIEVED',3,22)]
    rows = [[random.uniform(mn+(mx-mn)*0.1, mn+(mx-mn)*0.9) for _, mn, mx in dims] for _ in range(24)]
    hl = [32, 2400, 0.09, 0.94, 18]
    W, H, pad, top, bot = 1200, 260, 60, 36, 36
    colX = lambda i: pad + (i / (len(dims)-1 or 1)) * (W - pad*2)
    axes = "\n".join(
        f'<line x1="{colX(i):.1f}" y1="{top}" x2="{colX(i):.1f}" y2="{H-bot}" stroke="currentColor" stroke-width="1" stroke-opacity="0.2"/>'
        for i in range(len(dims))
    )
    def poly(row, op, color='currentColor'):
        pts = []
        for i, v in enumerate(row):
            _, mn, mx = dims[i]
            frac = (v - mn) / (mx - mn or 1)
            x = colX(i)
            y = top + (1 - max(0, min(1, frac))) * (H - top - bot)
            pts.append(f"{x:.1f},{y:.1f}")
        return f'<polyline points="{" ".join(pts)}" fill="none" stroke="{color}" stroke-width="1" stroke-opacity="{op}"/>'
    bodies = "\n".join(poly(r, '0.16') for r in rows)
    hl_svg = poly(hl, '1', 'var(--z-accent)')
    labels = []
    for i, (name, mn, mx) in enumerate(dims):
        x = colX(i)
        labels.append(f'<text x="{x:.1f}" y="22" fill="currentColor" font-family="var(--font-prose)" font-size="11" font-weight="700" letter-spacing="1.5" text-anchor="middle">{name}</text>')
        labels.append(f'<text x="{x:.1f}" y="{H-16}" fill="currentColor" opacity="0.55" font-family="var(--font-data)" font-size="10" text-anchor="middle">{fmt_num(mn)}</text>')
        labels.append(f'<text x="{x:.1f}" y="{H-4}" fill="currentColor" opacity="0.55" font-family="var(--font-data)" font-size="10" text-anchor="middle">{fmt_num(mx)}</text>')
    return f'''
    <svg viewBox="0 0 {W} {H}" preserveAspectRatio="none" style="width:100%; height:{H}px;">
      {axes}
      {bodies}
      {hl_svg}
      {chr(10).join(labels)}
    </svg>
    <div class="legend">
      <span class="mono faint">{len(rows)} rows · {len(dims)} dimensions · 1 highlighted</span>
    </div>'''

def attentionmap_html():
    tokens = [
        ('The',0.2),('XERJ',0.82),('engine',0.55),('stores',0.78),('billions',0.95),
        ('of',0.15),('vectors',0.88),('in',0.12),('a',0.08),('single',0.45),
        ('segment',0.72),('cache',0.68),('and',0.14),('returns',0.51),('hybrid',0.85),
        ('results',0.64),('in',0.10),('under',0.42),('40',0.98),('milliseconds.',0.60),
    ]
    mx = max(w for _, w in tokens)
    spans = []
    for t, w in tokens:
        op = max(0.08, w / mx)
        style = f'color:var(--z-accent); opacity:{op:.2f};' if w / mx > 0.82 else f'opacity:{op:.2f};'
        spans.append(f'<span style="{style}">{esc(t)}</span>')
    top3 = [t for t, _ in sorted(tokens, key=lambda x: -x[1])[:3]]
    return f'''
    <div class="attn">{' '.join(spans)}</div>
    <div class="legend">
      <span class="mono faint">peak attention on <span class="accent">{' · '.join(top3)}</span></span>
    </div>'''

# ============================================================
# LOGO BUILDERS
# ============================================================

def wordmark_svg(stroke_bottom='#f4f2ec', stroke_top='#ffc400',
                 xerj_fill='#f4f2ec', ai_fill='#ffc400', bg='#0b0b0d',
                 with_bg=True):
    bg_rect = f'<rect width="800" height="240" fill="{bg}"/>' if with_bg else ''
    return f'''<svg viewBox="0 0 800 240" preserveAspectRatio="xMidYMid meet" style="width:100%; display:block;">
  {bg_rect}
  <line x1="90" y1="200" x2="530" y2="200" stroke="{stroke_bottom}" stroke-width="1" stroke-linecap="square"/>
  <line x1="530" y1="40" x2="710" y2="40" stroke="{stroke_top}" stroke-width="1" stroke-linecap="square"/>
  <text x="530" y="170" font-family="'Big Shoulders Display','Inter',sans-serif" font-weight="900" font-size="140" letter-spacing="4" text-anchor="end" fill="{xerj_fill}">XERJ</text>
  <text x="530" y="170" font-family="'Big Shoulders Display','Inter',sans-serif" font-weight="900" font-size="140" letter-spacing="4" text-anchor="start" fill="{ai_fill}">.AI</text>
</svg>'''

def letter_mark_svg(fill='#ffc400', bg='#0b0b0d', with_bg=True):
    bg_rect = f'<rect width="200" height="200" fill="{bg}"/>' if with_bg else ''
    return f'''<svg viewBox="0 0 200 200" preserveAspectRatio="xMidYMid meet" style="width:100%; display:block;">
  {bg_rect}
  <line x1="30" y1="24" x2="170" y2="24" stroke="{fill}" stroke-width="1" stroke-linecap="square"/>
  <line x1="30" y1="170" x2="170" y2="170" stroke="{fill}" stroke-width="1" stroke-linecap="square"/>
  <text x="100" y="140" font-family="'Big Shoulders Display','Inter',sans-serif" font-weight="900" font-size="120" text-anchor="middle" fill="{fill}">X</text>
</svg>'''

def short_form_svg():
    return '''<svg viewBox="0 0 600 240" preserveAspectRatio="xMidYMid meet" style="width:100%; display:block;">
  <rect width="600" height="240" fill="#0b0b0d"/>
  <line x1="90" y1="40" x2="510" y2="40" stroke="#f4f2ec" stroke-width="1" stroke-linecap="square"/>
  <line x1="90" y1="200" x2="510" y2="200" stroke="#f4f2ec" stroke-width="1" stroke-linecap="square"/>
  <text x="300" y="170" font-family="'Big Shoulders Display','Inter',sans-serif" font-weight="900" font-size="140" letter-spacing="4" text-anchor="middle" fill="#f4f2ec">XERJ</text>
</svg>'''

def systemline_svg():
    return '''<svg viewBox="0 0 800 100" preserveAspectRatio="xMidYMid meet" style="width:100%; display:block;">
  <rect width="800" height="100" fill="#0b0b0d"/>
  <line x1="210" y1="78" x2="450" y2="78" stroke="#3a3836" stroke-width="1" stroke-linecap="square"/>
  <line x1="450" y1="22" x2="600" y2="22" stroke="#ffc400" stroke-width="1" stroke-linecap="square"/>
  <text x="400" y="58" font-family="'IBM Plex Sans','Inter',sans-serif" font-weight="600" font-size="20" letter-spacing="4.8" text-anchor="middle" fill="#f4f2ec">XERJ.AI · OBSERVE</text>
</svg>'''

# ============================================================
# DOCUMENT
# ============================================================

STYLE = r'''
@page {
  size: 8.5in 11in;
  margin: 0;
  background: #0b0b0d;
}
* { box-sizing: border-box; }
html, body {
  margin: 0; padding: 0;
  background: #0b0b0d;
  color: #f4f2ec;
  font-family: 'Inter', system-ui, sans-serif;
  font-size: 11pt;
  line-height: 1.5;
  -webkit-print-color-adjust: exact;
  print-color-adjust: exact;
}
:root {
  --z-bg:     #0b0b0d;
  --z-ink:    #f4f2ec;
  --z-mute:   #8a8680;
  --z-faint:  #2b2a28;
  --z-line:   #3a3836;
  --z-accent: #ffc400;
  --font-display: 'Big Shoulders Display', 'Inter', system-ui, sans-serif;
  --font-prose:   'Inter', system-ui, sans-serif;
  --font-data:    'IBM Plex Sans', 'Inter', system-ui, sans-serif;
  --font-mono:    'JetBrains Mono', 'IBM Plex Mono', monospace;
  --track-ui: 0.14em;
}

.page {
  width: 8.5in;
  height: 11in;
  padding: 0.6in 0.7in;
  page-break-after: always;
  page-break-inside: avoid;
  break-after: page;
  break-inside: avoid;
  background: #0b0b0d;
  position: relative;
  overflow: hidden;
  display: flex;
  flex-direction: column;
}
.page:last-child { page-break-after: auto; }

/* page header/footer — tiny signature bars */
.phead {
  display: flex; justify-content: space-between;
  font-family: var(--font-data);
  font-size: 8pt;
  letter-spacing: var(--track-ui);
  text-transform: uppercase;
  color: var(--z-mute);
  padding-bottom: 8pt;
  border-bottom: 1px solid var(--z-line);
  margin-bottom: 18pt;
}
.phead .right { color: var(--z-mute); }
.pfoot {
  margin-top: auto;
  padding-top: 10pt;
  border-top: 1px solid var(--z-line);
  display: flex; justify-content: space-between;
  font-family: var(--font-data);
  font-size: 8pt;
  letter-spacing: var(--track-ui);
  text-transform: uppercase;
  color: var(--z-mute);
}
.pfoot .pgnum { color: var(--z-ink); font-weight: 600; }

/* typography scale */
h1.cover-title {
  font-family: var(--font-display); font-weight: 900;
  font-size: 72pt; line-height: 0.9; letter-spacing: -0.01em;
  margin: 0;
}
h1.ch-num {
  font-family: var(--font-display); font-weight: 900;
  font-size: 260pt; line-height: 1; letter-spacing: -0.03em;
  margin: 0; color: var(--z-accent);
}
h1.ch-title {
  font-family: var(--font-display); font-weight: 900;
  font-size: 60pt; line-height: 0.9; letter-spacing: -0.01em;
  margin: 0 0 6pt;
}
h2.scene {
  font-family: var(--font-display); font-weight: 900;
  font-size: 32pt; line-height: 0.95; letter-spacing: -0.005em;
  margin: 0 0 10pt;
}
h3 {
  font-family: var(--font-display); font-weight: 900;
  font-size: 18pt; line-height: 1.1; letter-spacing: 0.005em;
  margin: 14pt 0 8pt;
}
h4 {
  font-family: var(--font-data); font-weight: 700;
  font-size: 9pt; letter-spacing: var(--track-ui);
  text-transform: uppercase; color: var(--z-mute);
  margin: 10pt 0 6pt;
}
p { margin: 0 0 8pt; font-size: 10.5pt; line-height: 1.55; max-width: 6.8in; }
p.lead { font-size: 13pt; line-height: 1.45; max-width: 6.2in; }
.kicker {
  font-family: var(--font-data); font-weight: 700;
  font-size: 8pt; letter-spacing: var(--track-ui);
  text-transform: uppercase; color: var(--z-mute);
  display: flex; gap: 8pt; align-items: center; flex-wrap: wrap;
}
.kicker .dash { color: var(--z-faint); }
.kicker.big { font-size: 10pt; color: var(--z-ink); letter-spacing: 0.18em; }

.accent { color: var(--z-accent); }
.mute   { color: var(--z-mute); }
.mono   { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
.faint  { color: var(--z-mute); }

.rule { border-bottom: 1px solid var(--z-line); margin: 14pt 0; }

/* LOGO DISPLAY FRAMES */
.lockup {
  border: 1px solid var(--z-line);
  padding: 24pt;
  margin: 12pt 0;
  position: relative;
}
.lockup[data-bg="day"]   { background: #f6f4ee; color: #11120f; }
.lockup[data-bg="paper"] { background: #f4f2ec; color: #0b0b0d; }
.lockup .cap {
  position: absolute; top: 6pt; left: 10pt;
  font-family: var(--font-data); font-size: 7.5pt;
  letter-spacing: var(--track-ui); color: var(--z-mute);
  text-transform: uppercase;
}
.lockup[data-bg="day"] .cap, .lockup[data-bg="paper"] .cap { color: #696762; }

/* 2-up / 4-up grids */
.grid-2 { display: grid; grid-template-columns: 1fr 1fr; gap: 12pt; }
.grid-3 { display: grid; grid-template-columns: 1fr 1fr 1fr; gap: 12pt; }

/* CHART TOKENS */
.chart-card {
  padding: 12pt 0;
  border-top: 1px solid var(--z-line);
  border-bottom: 1px solid var(--z-line);
  color: var(--z-ink);
  margin-top: 10pt;
}
.legend {
  display: flex; justify-content: space-between;
  font-family: var(--font-data); font-size: 9pt;
  letter-spacing: var(--track-ui);
  text-transform: uppercase;
  color: var(--z-mute);
  margin-top: 8pt;
}
.legend .mid { flex: 1; text-align: center; }
.legend .mono { color: var(--z-ink); text-transform: none; letter-spacing: 0; font-family: var(--font-mono); }

/* Top-N */
.topn-list .row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 70pt minmax(0, 1fr) 44pt;
  gap: 8pt; align-items: baseline;
  padding: 5pt 0;
  border-bottom: 1px solid var(--z-faint);
  font-family: var(--font-data);
  font-size: 9.5pt;
  font-variant-numeric: tabular-nums;
}
.topn-list .row__label { color: var(--z-ink); overflow:hidden; text-overflow: ellipsis; white-space: nowrap; }
.topn-list .row__val   { text-align: right; font-weight: 700; color: var(--z-ink); }
.topn-list .row__bar   { color: var(--z-accent); }
.topn-list .row__bar svg { width: 100%; height: 4pt; }
.topn-list .row__pct   { text-align: right; color: var(--z-mute); font-size: 8pt; }

/* Dist legend */
.dist-legend {
  display: flex; flex-wrap: wrap; gap: 14pt 26pt; margin-top: 10pt;
}
.dist-legend .stack { display: flex; flex-direction: column; gap: 2pt; min-width: 64pt; }
.dist-legend .key   { font-family: var(--font-data); font-size: 8pt; letter-spacing: var(--track-ui); color: var(--z-mute); text-transform: uppercase; }
.dist-legend .mono  { font-family: var(--font-mono); color: var(--z-ink); }
.dist-legend .mono.big { font-size: 15pt; font-weight: 700; }
.dist-legend .mono.faint { color: var(--z-mute); font-size: 9pt; }

/* Heatmap */
.heatmap {
  font-family: var(--font-mono); font-size: 8.5pt; line-height: 1.7;
  white-space: nowrap;
}
.heatmap-row { display: block; white-space: nowrap; }
.heatmap-row .row-label {
  display: inline-block; width: 70pt;
  font-family: var(--font-data); font-size: 7.5pt;
  letter-spacing: var(--track-ui); color: var(--z-mute);
  text-transform: uppercase;
}
.heatmap-row .hc {
  display: inline-block; width: 28pt; text-align: right;
}
.heatmap-row.heatmap-head .hc { color: var(--z-mute); }

/* Multiples grid */
.mult-grid {
  display: grid; grid-template-columns: 1fr 1fr 1fr 1fr; gap: 12pt 14pt;
  margin-top: 8pt;
}
.mult-cell { color: var(--z-ink); }
.mult-cell .mult-head {
  display: flex; justify-content: space-between; align-items: baseline;
  margin-bottom: 3pt;
  font-family: var(--font-data); font-size: 8pt;
  letter-spacing: var(--track-ui);
  text-transform: uppercase; color: var(--z-mute);
}
.mult-cell svg { width: 100%; height: 22px; display: block; }

/* Stacked */
.stk-wrap .stk-row {
  display: grid; grid-template-columns: 40pt 1fr 60pt;
  gap: 12pt; align-items: center;
  padding: 5pt 0;
  border-bottom: 1px solid var(--z-faint);
  font-family: var(--font-data); font-size: 9pt;
}
.stk-lbl { color: var(--z-mute); letter-spacing: var(--track-ui); }
.stk-legend {
  margin-top: 6pt;
  font-family: var(--font-data); font-size: 7.5pt;
  letter-spacing: var(--track-ui); color: var(--z-mute);
  text-transform: uppercase;
}

/* Events */
.ev-list {}
.ev-row {
  display: grid; grid-template-columns: 80pt 40pt 1fr;
  gap: 10pt; padding: 4pt 0;
  border-bottom: 1px solid var(--z-faint);
  font-family: var(--font-mono); font-size: 8.5pt;
  font-variant-numeric: tabular-nums;
}
.ev-at { color: var(--z-mute); }
.ev-sev { font-family: var(--font-data); font-size: 7.5pt; letter-spacing: var(--track-ui); text-transform: uppercase; }
.ev-msg { color: var(--z-ink); overflow:hidden; text-overflow: ellipsis; white-space: nowrap; }

/* Flowband */
.fb-grid {
  display: grid; grid-template-columns: repeat(5, minmax(0, 1fr));
  gap: 8pt 14pt; margin-top: 8pt;
}
.fb-lbl .fb-name {
  font-family: var(--font-data); font-size: 8pt;
  letter-spacing: var(--track-ui); text-transform: uppercase;
}

/* Attention */
.attn {
  font-family: var(--font-data); font-size: 13pt; line-height: 1.85;
  max-width: 6.6in;
  color: var(--z-ink);
}
.attn .accent { color: var(--z-accent); }

/* Color swatches */
.swatch-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 16pt; margin-top: 10pt; }
.swatch {
  display: grid; grid-template-columns: 60pt 1fr 80pt;
  gap: 10pt; align-items: center;
  padding: 6pt 0;
  border-bottom: 1px solid var(--z-faint);
}
.swatch .chip {
  width: 60pt; height: 36pt;
  border: 1px solid var(--z-line);
}
.swatch .name { color: var(--z-ink); font-family: var(--font-data); font-size: 9pt; letter-spacing: var(--track-ui); text-transform: uppercase; }
.swatch .hex  { color: var(--z-mute); font-family: var(--font-mono); font-size: 9pt; text-align: right; }

/* Typeface rows */
.tf-row { padding: 10pt 0; border-bottom: 1px solid var(--z-faint); }
.tf-row .tf-name { font-family: var(--font-data); font-size: 8pt; letter-spacing: var(--track-ui); color: var(--z-mute); text-transform: uppercase; margin-bottom: 4pt; }

/* Scale ladder */
.scale-row { display: grid; grid-template-columns: 60pt 1fr 120pt; gap: 14pt; align-items: baseline; padding: 6pt 0; border-bottom: 1px solid var(--z-faint); }
.scale-row .px { font-family: var(--font-mono); font-size: 9pt; color: var(--z-mute); }
.scale-row .use { font-family: var(--font-data); font-size: 8pt; letter-spacing: var(--track-ui); text-transform: uppercase; color: var(--z-mute); text-align: right; }

/* Do/Dont */
.verdict { display: grid; grid-template-columns: 50pt 1fr; gap: 12pt; padding: 8pt 0; border-bottom: 1px solid var(--z-faint); }
.verdict .tag { font-family: var(--font-data); font-size: 8pt; letter-spacing: var(--track-ui); text-transform: uppercase; }
.verdict .tag.yes { color: var(--z-accent); }
.verdict .tag.no  { color: var(--z-mute); }
.verdict .body    { font-family: var(--font-prose); font-size: 10pt; color: var(--z-ink); line-height: 1.5; }

/* Size ladder cell for logo */
.ladder-row { display: grid; grid-template-columns: 80pt 1fr 120pt; gap: 14pt; align-items: center; padding: 6pt 0; border-bottom: 1px solid var(--z-faint); }
.ladder-row .px { font-family: var(--font-mono); font-size: 8pt; color: var(--z-mute); }
.ladder-row .use { font-family: var(--font-data); font-size: 8pt; letter-spacing: var(--track-ui); text-transform: uppercase; color: var(--z-mute); text-align: right; }
.ladder-row .ex { display: flex; align-items: baseline; }

/* Contents */
.toc-row {
  display: grid; grid-template-columns: 32pt 1fr 40pt;
  gap: 14pt; align-items: baseline;
  padding: 10pt 0;
  border-bottom: 1px solid var(--z-faint);
}
.toc-num { font-family: var(--font-mono); color: var(--z-accent); font-size: 11pt; font-weight: 700; }
.toc-title { font-family: var(--font-display); font-weight: 900; font-size: 22pt; letter-spacing: 0.005em; }
.toc-page { font-family: var(--font-mono); color: var(--z-mute); font-size: 10pt; text-align: right; font-variant-numeric: tabular-nums; }

/* Narrative */
.three-principles {
  display: grid; grid-template-columns: 1fr; gap: 14pt;
}
.principle {
  padding-top: 14pt;
  border-top: 1px solid var(--z-line);
}

/* 6-row dashboard infographic */
.infog {
  border-top: 1px solid var(--z-line);
  border-bottom: 1px solid var(--z-line);
  margin-top: 12pt;
}
.infog .row {
  display: grid; grid-template-columns: 1fr 80pt 60pt;
  gap: 12pt; align-items: baseline;
  padding: 7pt 0; border-bottom: 1px solid var(--z-faint);
  font-family: var(--font-data); font-size: 9pt;
  letter-spacing: var(--track-ui); text-transform: uppercase;
}
.infog .row:last-child { border-bottom: 0; }
.infog .row .k { color: var(--z-mute); }
.infog .row .v { font-family: var(--font-mono); color: var(--z-ink); text-align: right; letter-spacing: 0; text-transform: none; }
.infog .row .gold { color: var(--z-accent); text-align: right; font-family: var(--font-mono); }

/* Download table */
.dl-list {}
.dl-row {
  display: grid; grid-template-columns: 120pt 1fr 80pt;
  gap: 12pt; align-items: baseline;
  padding: 7pt 0;
  border-bottom: 1px solid var(--z-faint);
  font-family: var(--font-data); font-size: 9pt;
  letter-spacing: var(--track-ui); text-transform: uppercase;
}
.dl-row .name { color: var(--z-ink); }
.dl-row .desc { color: var(--z-mute); text-transform: none; letter-spacing: 0; font-family: var(--font-prose); font-size: 9pt; }
.dl-row .get  { color: var(--z-accent); text-align: right; }

/* Rule diagram (clear space) */
.cs-demo {
  border: 1px solid var(--z-line);
  padding: 36pt 24pt;
  margin-top: 14pt;
  background: var(--z-bg);
}
.cs-demo svg { width: 100%; height: auto; display: block; }
'''


def page_header(kicker, ch):
    return f'''  <div class="phead"><span>XERJ.AI · BRAND BOOK · V1</span><span class="right">{esc(kicker)}</span></div>'''

def page_footer(pgnum, section):
    return f'''  <div class="pfoot"><span>{esc(section)}</span><span class="pgnum">{pgnum:02d}</span></div>'''


# ============================================================
# PAGE BUILDERS
# ============================================================

pages = []

# --- P1 · COVER ---
pages.append(f'''<div class="page" style="padding:0.7in; display:flex; flex-direction:column; justify-content:space-between;">
  <!-- TOP ROW · kicker left · wordmark pinned top-right as the identifier -->
  <div style="display:flex; justify-content:space-between; align-items:flex-start; gap:30pt;">
    <div class="kicker big" style="padding-top:6pt;"><span>BRAND BOOK</span><span class="dash">·</span><span>V1</span><span class="dash">·</span><span>2026·04</span></div>
    <div style="width:2.6in; flex-shrink:0;">
      {wordmark_svg()}
    </div>
  </div>

  <!-- CENTER · the book title IS the cover -->
  <div>
    <h1 class="cover-title">BRAND<br>BOOK.</h1>
    <p class="lead" style="color:var(--z-mute); max-width:4.6in; margin-top:18pt;">
      The rules that carry the XERJ.AI identity across every
      surface — logo, colour, typography, UX, voice — codified
      in one place.
    </p>
  </div>

  <!-- BOTTOM · issuance line -->
  <div style="display:flex; justify-content:space-between; font-family:var(--font-data); font-size:9pt; letter-spacing:var(--track-ui); text-transform:uppercase; color:var(--z-mute); padding-top:10pt; border-top:1px solid var(--z-line);">
    <span>ISSUED BY · XERJ.AI DESIGN</span>
    <span>XERJ.AI · OBSERVE</span>
  </div>
</div>''')

# --- P2 · COLOPHON ---
pages.append(f'''<div class="page">
{page_header("COLOPHON", "")}
  <div style="flex:1; display:flex; flex-direction:column; justify-content:center; max-width:5.8in;">
    <h2 class="scene">THIS BOOK<br>CODIFIES THE<br><span class="accent">LANGUAGE.</span></h2>
    <p class="lead">
      XERJ is a storage system. It looks like a storage system.
      The product is typography, negative space, and 1&thinsp;px
      lines — and this document is the rulebook that keeps it that
      way.
    </p>
    <p>
      Seven chapters. Logo, colour, typography, UX and charts,
      philosophy, voice, and resources. Every rule herein is
      non-negotiable. A designer who wants to break one is welcome
      to file a 500-word essay explaining why, and if we ship the
      exception it becomes a new rule.
    </p>
    <div class="rule"></div>
    <div class="kicker"><span>ISSUED BY</span><span class="dash">·</span><span>XERJ.AI DESIGN</span></div>
    <div class="kicker"><span>VERSION</span><span class="dash">·</span><span>1.0 · 2026-04</span></div>
    <div class="kicker"><span>REPLACES</span><span class="dash">·</span><span>—</span></div>
    <div class="kicker"><span>OWNER</span><span class="dash">·</span><span>BRAND · XERJ.AI</span></div>
  </div>
{page_footer(2, "COLOPHON")}
</div>''')

# --- P3 · CONTENTS ---
toc = [
    (1, 'LOGO',        'The word is the mark.',         4),
    (2, 'COLOR',       'Gold is earned.',              13),
    (3, 'TYPOGRAPHY',  'Four typefaces. Seven sizes.', 17),
    (4, 'UX & DATA',   'Type is the infographic.',     21),
    (5, 'PHILOSOPHY',  'Why it looks like this.',      33),
    (6, 'VOICE',       'Solid. Quiet. Technical.',     37),
    (7, 'RESOURCES',   'Downloads and references.',    39),
]
toc_rows = "\n".join(
    f'''  <div class="toc-row">
    <span class="toc-num">0{n}</span>
    <span class="toc-title">{title}<span style="font-family:var(--font-prose); font-weight:400; font-size:11pt; color:var(--z-mute); margin-left:10pt;">{sub}</span></span>
    <span class="toc-page">p. {pg:02d}</span>
  </div>''' for n, title, sub, pg in toc)
pages.append(f'''<div class="page">
{page_header("CONTENTS", "")}
  <h2 class="scene" style="margin-bottom:14pt;">CONTENTS.</h2>
  {toc_rows}
{page_footer(3, "CONTENTS")}
</div>''')

# ============================================================
# CHAPTER 01 · LOGO
# ============================================================

# --- P4 · CH01 DIVIDER ---
pages.append(f'''<div class="page" style="display:flex; flex-direction:column; justify-content:space-between;">
{page_header("CHAPTER 01 · LOGO", "01")}
  <div style="flex:1; display:flex; flex-direction:column; justify-content:center;">
    <h1 class="ch-num">01</h1>
    <h1 class="ch-title">LOGO.</h1>
    <p class="lead" style="max-width:5.6in; margin-top:6pt;">
      The word is the mark. Two 1&thinsp;px rules carry the story —
      a paper-coloured line under XERJ, a yellow line over .AI.
      Storage on the bottom. AI on top.
    </p>
  </div>
{page_footer(4, "CHAPTER 01 · LOGO")}
</div>''')

# --- P5 · PRIMARY WORDMARK ---
pages.append(f'''<div class="page">
{page_header("01 · LOGO · PRIMARY", "01")}
  <div class="kicker"><span>01</span><span class="dash">·</span><span>PRIMARY WORDMARK</span></div>
  <h2 class="scene">XERJ<span class="accent">.AI</span></h2>
  <p>
    The primary lockup. Root word set in paper (<span class="mono">#F4F2EC</span>)
    on night (<span class="mono">#0B0B0D</span>). Suffix
    <span class="accent">.AI</span> in accent (<span class="mono">#FFC400</span>).
    Paper line under XERJ (the stored record). Yellow line over .AI
    (the answer served back).
  </p>

  <div class="lockup" data-bg="night" style="padding:40pt;">
    <span class="cap">01 · NIGHT · PRIMARY</span>
    <div style="width:100%; max-width:5.4in; margin:0 auto;">
      {wordmark_svg()}
    </div>
  </div>

  <h4>CONSTRUCTION</h4>
  <p>
    Typeface: <span class="mono">Big Shoulders Display 900</span>.
    Tracking: <span class="mono">+0.24 em</span>. Both rules are
    exactly <span class="mono">1&thinsp;px</span>, placed
    <span class="mono">30&thinsp;px</span> from baseline
    (<span class="mono">y=170</span>) and
    <span class="mono">30&thinsp;px</span> from cap-top
    (<span class="mono">y=70</span>). Measured, not optical.
  </p>
{page_footer(5, "CHAPTER 01 · LOGO")}
</div>''')

# --- P6 · RULE OF COLOUR ---
pages.append(f'''<div class="page">
{page_header("01 · LOGO · RULE OF COLOUR", "01")}
  <div class="kicker"><span>01</span><span class="dash">·</span><span>RULE OF COLOUR</span></div>
  <h2 class="scene">STORED <span style="color:var(--z-ink);">BELOW.</span><br>SERVED <span class="accent">ABOVE.</span></h2>
  <p class="lead">
    A rule always takes the colour of the text it touches.
    The paper line under <span style="color:var(--z-ink);">XERJ</span> is paper.
    The yellow line over <span class="accent">.AI</span> is yellow.
    Never cross-colour. Never extend one rule past the boundary to
    underline both halves.
  </p>

  <div class="lockup" data-bg="night">
    <span class="cap">DETAIL · THE TWO RULES MEET AT THE DOT</span>
    <div style="width:100%; max-width:5.6in; margin:14pt auto 0;">
      {wordmark_svg()}
    </div>
  </div>

  <p>
    Each rule owns exactly its half of the record. The offset
    between them — one above, one below — reads as
    "storage → retrieval" in a single glance. That diagonal motion
    is the mark's identity.
  </p>
{page_footer(6, "CHAPTER 01 · LOGO")}
</div>''')

# --- P7 · SHORT FORM + LETTER MARK + SYSTEM LINE ---
pages.append(f'''<div class="page">
{page_header("01 · LOGO · SHORTER FORMS", "01")}
  <div class="kicker"><span>01</span><span class="dash">·</span><span>SHORTER FORMS</span></div>
  <h2 class="scene">SHORT.<br><span class="accent">SHORTER.</span></h2>
  <p>
    When space is tight the wordmark drops the suffix. At favicon
    scale only the X remains. The rule system simplifies: both
    rules take the colour of whatever text is left.
  </p>

  <div class="grid-2">
    <div class="lockup" data-bg="night">
      <span class="cap">02A · SHORT FORM · PAPER RULES</span>
      <div style="width:100%; max-width:3.6in; margin:14pt auto 0;">
        {short_form_svg()}
      </div>
    </div>
    <div class="lockup" data-bg="night">
      <span class="cap">02B · LETTER MARK · THE KEY</span>
      <div style="width:1.6in; margin:14pt auto 0;">
        {letter_mark_svg()}
      </div>
    </div>
  </div>

  <div class="lockup" data-bg="night" style="margin-top:14pt;">
    <span class="cap">03 · SYSTEM LINE · TRACKED SIGNATURE</span>
    <div style="width:100%; max-width:5.2in; margin:14pt auto 0;">
      {systemline_svg()}
    </div>
  </div>

  <p>
    Approved second-tag variants:
    <span class="mono">OBSERVE</span> (default),
    <span class="mono">INDEX</span> (docs),
    <span class="mono">EXPLAIN</span> (compliance),
    <span class="mono">QUERY</span> (product dashboards).
    One tag per surface. Never stacked.
  </p>
{page_footer(7, "CHAPTER 01 · LOGO")}
</div>''')

# --- P8 · COLOR TREATMENTS (4 lockups) ---
pages.append(f'''<div class="page">
{page_header("01 · LOGO · COLOUR TREATMENTS", "01")}
  <div class="kicker"><span>01</span><span class="dash">·</span><span>COLOUR TREATMENTS</span></div>
  <h2 class="scene">FOUR SURFACES.<br>ONE <span class="accent">MARK.</span></h2>

  <div class="grid-2">
    <div class="lockup" data-bg="night">
      <span class="cap">04A · NIGHT · #0B0B0D</span>
      <div style="width:100%; max-width:3.3in; margin:12pt auto 0;">{wordmark_svg()}</div>
    </div>
    <div class="lockup" data-bg="day">
      <span class="cap">04B · DAY · #F6F4EE</span>
      <div style="width:100%; max-width:3.3in; margin:12pt auto 0;">{wordmark_svg(stroke_bottom="#11120f", stroke_top="#a06800", xerj_fill="#11120f", ai_fill="#a06800", bg="#f6f4ee")}</div>
    </div>
    <div class="lockup" data-bg="paper">
      <span class="cap">04C · PAPER · PRINT</span>
      <div style="width:100%; max-width:3.3in; margin:12pt auto 0;">{wordmark_svg(stroke_bottom="#0b0b0d", stroke_top="#a06800", xerj_fill="#0b0b0d", ai_fill="#a06800", bg="#f4f2ec")}</div>
    </div>
    <div class="lockup" data-bg="night">
      <span class="cap">04D · MONO · OUTLINE</span>
      <div style="width:100%; max-width:3.3in; margin:12pt auto 0;">{wordmark_svg(stroke_bottom="currentColor", stroke_top="currentColor", xerj_fill="currentColor", ai_fill="currentColor", bg="#0b0b0d")}</div>
    </div>
  </div>

  <p style="margin-top:10pt;">
    Night is primary. Day is a courtesy. Paper is for print. Mono
    (<span class="mono">currentColor</span>) is the fallback for
    decks and partner surfaces we do not control — we do not ship
    neon yellow on someone else's background.
  </p>
{page_footer(8, "CHAPTER 01 · LOGO")}
</div>''')

# --- P9 · CLEAR SPACE ---
pages.append(f'''<div class="page">
{page_header("01 · LOGO · CLEAR SPACE", "01")}
  <div class="kicker"><span>01</span><span class="dash">·</span><span>CLEAR SPACE</span></div>
  <h2 class="scene">CLEAR SPACE<br>= HEIGHT OF <span class="accent">X.</span></h2>
  <p>
    Minimum padding around the wordmark on every side is equal to
    the cap height of X at whatever size the mark is rendered.
    Absolute — no edge of the wordmark ever sits closer than one X
    to a container border, photo crop, another logo, or interface
    chrome.
  </p>

  <div class="cs-demo">
    <svg viewBox="0 0 680 320" preserveAspectRatio="xMidYMid meet" style="color: var(--z-line);">
      <rect x="20" y="20" width="640" height="280" fill="none" stroke="currentColor" stroke-width="1"/>
      <rect x="120" y="100" width="440" height="120" fill="none" stroke="currentColor" stroke-width="1" stroke-dasharray="2 4"/>
      <line x1="150" y1="210" x2="370" y2="210" stroke="var(--z-ink)" stroke-width="1" stroke-linecap="square"/>
      <line x1="370" y1="130" x2="520" y2="130" stroke="var(--z-accent)" stroke-width="1" stroke-linecap="square"/>
      <text x="370" y="190" font-family="'Big Shoulders Display','Inter',sans-serif" font-weight="900" font-size="86" letter-spacing="2" text-anchor="end" fill="var(--z-ink)">XERJ</text>
      <text x="370" y="190" font-family="'Big Shoulders Display','Inter',sans-serif" font-weight="900" font-size="86" letter-spacing="2" text-anchor="start" fill="var(--z-accent)">.AI</text>
      <line x1="20" y1="100" x2="120" y2="100" stroke="currentColor" stroke-width="1"/>
      <line x1="20" y1="220" x2="120" y2="220" stroke="currentColor" stroke-width="1"/>
      <line x1="70" y1="20"  x2="70" y2="100" stroke="currentColor" stroke-width="1"/>
      <line x1="70" y1="220" x2="70" y2="300" stroke="currentColor" stroke-width="1"/>
      <text x="50" y="62"  font-family="'JetBrains Mono',monospace" font-size="12" fill="var(--z-mute)">1 X</text>
      <text x="50" y="266" font-family="'JetBrains Mono',monospace" font-size="12" fill="var(--z-mute)">1 X</text>
      <text x="72" y="164" font-family="'JetBrains Mono',monospace" font-size="12" fill="var(--z-mute)">X HT</text>
    </svg>
  </div>
{page_footer(9, "CHAPTER 01 · LOGO")}
</div>''')

# --- P10 · SIZE LADDER ---
def sized_wordmark(font_size=140, box_w=560, box_h=240, letter_spacing=4, with_rules=True):
    # Scale svg to keep rules visually consistent vs. wordmark height
    return f'''<div class="ex"><svg viewBox="0 0 800 240" preserveAspectRatio="xMidYMid meet" style="width:{box_w}pt; max-width:100%; height:{box_h*0.18}pt;">
  <rect width="800" height="240" fill="#0b0b0d"/>
  {'<line x1="90" y1="200" x2="530" y2="200" stroke="#f4f2ec" stroke-width="1" stroke-linecap="square"/>' if with_rules else ''}
  {'<line x1="530" y1="40" x2="710" y2="40" stroke="#ffc400" stroke-width="1" stroke-linecap="square"/>' if with_rules else ''}
  <text x="530" y="170" font-family="'Big Shoulders Display','Inter',sans-serif" font-weight="900" font-size="140" letter-spacing="{letter_spacing}" text-anchor="end" fill="#f4f2ec">XERJ</text>
  <text x="530" y="170" font-family="'Big Shoulders Display','Inter',sans-serif" font-weight="900" font-size="140" letter-spacing="{letter_spacing}" text-anchor="start" fill="#ffc400">.AI</text>
</svg></div>'''

pages.append(f'''<div class="page">
{page_header("01 · LOGO · SIZE LADDER", "01")}
  <div class="kicker"><span>01</span><span class="dash">·</span><span>SIZE LADDER</span></div>
  <h2 class="scene">THE LADDER.<br>WHAT FITS <span class="accent">WHERE.</span></h2>
  <p>
    Every surface has a pre-approved size. Below the minimum the
    wordmark drops to short form, then to letter mark, then it is
    removed. The mark is never rendered below 16&thinsp;px cap
    height.
  </p>

  <div class="ladder-row"><span class="px">96 PT · DUAL RULES</span><span class="ex" style="font-family:var(--font-display); font-weight:900; font-size:60pt; letter-spacing:0.025em; line-height:1;">XERJ<span class="accent">.AI</span></span><span class="use">COVER · SLIDE TITLE</span></div>
  <div class="ladder-row"><span class="px">56 PT · DUAL RULES</span><span class="ex" style="font-family:var(--font-display); font-weight:900; font-size:36pt; letter-spacing:0.03em; line-height:1;">XERJ<span class="accent">.AI</span></span><span class="use">SECTION · DOC HEAD</span></div>
  <div class="ladder-row"><span class="px">32 PT · NO MARKS</span><span class="ex" style="font-family:var(--font-display); font-weight:900; font-size:22pt; letter-spacing:0.04em; line-height:1;">XERJ<span class="accent">.AI</span></span><span class="use">NAV · APP HEADER</span></div>
  <div class="ladder-row"><span class="px">20 PT · NO MARKS</span><span class="ex" style="font-family:var(--font-display); font-weight:900; font-size:14pt; letter-spacing:0.05em; line-height:1;">XERJ<span class="accent">.AI</span></span><span class="use">FOOTER · SIGNATURE</span></div>
  <div class="ladder-row"><span class="px">16 PT · MIN</span><span class="ex" style="font-family:var(--font-display); font-weight:900; font-size:11pt; letter-spacing:0.05em; line-height:1;">XERJ</span><span class="use">SHORT FORM ONLY</span></div>
  <div class="ladder-row"><span class="px">&lt; 16 PT</span><span class="ex accent" style="font-family:var(--font-display); font-weight:900; font-size:11pt; line-height:1;">X</span><span class="use">LETTER MARK · FAVICON</span></div>

  <p style="margin-top:10pt; color:var(--z-mute); font-size:9pt;">
    The rules are drawn at a fixed 1&thinsp;px weight regardless
    of wordmark size. Below 56&thinsp;pt the rules start to compete
    with the letterforms and drop off entirely.
  </p>
{page_footer(10, "CHAPTER 01 · LOGO")}
</div>''')

# --- P11 · DO (rules to follow) ---
dos = [
    "Set the wordmark in Big Shoulders Display 900, all caps, +0.24\u2009em tracking. Accent the <span class=\"accent\">.AI</span> suffix only.",
    "Place on a solid night (<span class=\"mono\">#0B0B0D</span>) or solid day (<span class=\"mono\">#F6F4EE</span>) surface. Nothing in between.",
    "Keep one full X-height of clear space on every side — including above and below.",
    "Drop to the short form at 16&thinsp;pt, then to the letter mark, then omit the logo entirely rather than shrink it further.",
    "Colour each rule to match the text it touches — paper under XERJ, yellow over .AI. The rule <em>is</em> an extension of the glyph colour, not a separate ornament.",
    "Draw rules at true 1&thinsp;px with <span class=\"mono\">stroke-linecap: square</span>. Position them 30&thinsp;px from baseline and 30&thinsp;px from cap-top — measured, not eyeballed.",
]
do_rows = "\n".join(f'  <div class="verdict"><span class="tag yes">DO</span><span class="body">{d}</span></div>' for d in dos)

pages.append(f'''<div class="page">
{page_header("01 · LOGO · DO", "01")}
  <div class="kicker"><span>01</span><span class="dash">·</span><span>DO</span></div>
  <h2 class="scene">WHAT <span class="accent">SHIPS.</span></h2>
{do_rows}
{page_footer(11, "CHAPTER 01 · LOGO")}
</div>''')

# --- P12 · DON'T ---
donts = [
    "Outline, stroke, emboss, or add a drop shadow. The mark is a solid typographic form. One weight, zero effects.",
    "Tint the root word. <span class=\"mono\">XERJ</span> is always paper-on-night or ink-on-day. Never yellow, never red, never blue.",
    "Place the wordmark on photography, gradients, screenshots, or any surface with more than 5&thinsp;% visual texture.",
    "Rotate, skew, stretch, condense, or redraw the letterforms. If it is not Big Shoulders 900 at +0.24&thinsp;em, it is not the wordmark.",
    "Lock the wordmark up with an icon, mascot, illustration, or partner logo without a full X-height gap between them.",
    "Extend one rule past the XERJ/.AI boundary to underline the whole wordmark — that collapses the two-colour story into a single underline.",
    "Cross-colour the rules (yellow line under XERJ, paper line over .AI). The rule always matches the text it connects to, never the opposite half.",
    "Put both rules on the same side of the wordmark. The rule over .AI must be above cap-top; the rule under XERJ must be below baseline. Never co-planar.",
]
dont_rows = "\n".join(f'  <div class="verdict"><span class="tag no">DON\'T</span><span class="body">{d}</span></div>' for d in donts)

pages.append(f'''<div class="page">
{page_header("01 · LOGO · DON\'T", "01")}
  <div class="kicker"><span>01</span><span class="dash">·</span><span>DON\'T</span></div>
  <h2 class="scene">WHAT <span class="accent">DOESN\'T.</span></h2>
{dont_rows}
{page_footer(12, "CHAPTER 01 · LOGO")}
</div>''')

# ============================================================
# Keep going — chapter 02 (color), 03 (typography), 04 (UX),
# 05 (philosophy), 06 (voice), 07 (resources)
# ============================================================

# --- P13 · BREAK PAGE / CHAPTER END? Actually let's add a transitional page.
# Skip breathing page, go straight to chapter 02 divider.

# --- P13 · CHAPTER 02 DIVIDER ---
pages.append(f'''<div class="page" style="display:flex; flex-direction:column; justify-content:space-between;">
{page_header("CHAPTER 02 · COLOR", "02")}
  <div style="flex:1; display:flex; flex-direction:column; justify-content:center;">
    <h1 class="ch-num">02</h1>
    <h1 class="ch-title">COLOR.</h1>
    <p class="lead" style="max-width:5.6in; margin-top:6pt;">
      Five colours. One of them is yellow. It is a gold standard,
      not a mood — it is applied only where something has earned it.
    </p>
  </div>
{page_footer(13, "CHAPTER 02 · COLOR")}
</div>''')

# --- P14 · NIGHT PALETTE ---
night_rows = [
    ('INK · BG',            '#0B0B0D', '#0B0B0D'),
    ('PAPER · INK',         '#F4F2EC', '#F4F2EC'),
    ('MUTE',                '#8A8680', '#8A8680'),
    ('FAINT · HAIRLINES',   '#2B2A28', '#2B2A28'),
    ('LINE',                '#3A3836', '#3A3836'),
    ('ACCENT · SOLID',      '#FFC400', '#FFC400'),
]
night_html = "\n".join(f'''  <div class="swatch">
    <span class="chip" style="background:{swatch};"></span>
    <span class="name">{name}</span>
    <span class="hex">{hex}</span>
  </div>''' for name, swatch, hex in night_rows)

pages.append(f'''<div class="page">
{page_header("02 · COLOR · NIGHT", "02")}
  <div class="kicker"><span>02</span><span class="dash">·</span><span>NIGHT · PRIMARY</span></div>
  <h2 class="scene">NIGHT<br>IS <span class="accent">PRIMARY.</span></h2>
  <p>
    Night is the working palette. The product is read in dim rooms
    at 2&nbsp;am when something broke. Every shade below has been
    tuned to print as 1&thinsp;px lines on ink bg and still hold
    contrast without glow.
  </p>
  <div style="margin-top:14pt;">
{night_html}
  </div>
{page_footer(14, "CHAPTER 02 · COLOR")}
</div>''')

# --- P15 · DAY PALETTE + ACCENT RULE ---
day_rows = [
    ('PAPER · BG',        '#F6F4EE', '#F6F4EE'),
    ('INK · INK',         '#11120F', '#11120F'),
    ('MUTE',              '#696762', '#696762'),
    ('FAINT · HAIRLINES', '#CFCBBF', '#CFCBBF'),
    ('ACCENT · DAY',      '#A06800', '#A06800'),
]
day_html = "\n".join(f'''  <div class="swatch">
    <span class="chip" style="background:{swatch};"></span>
    <span class="name">{name}</span>
    <span class="hex">{hex}</span>
  </div>''' for name, swatch, hex in day_rows)

pages.append(f'''<div class="page">
{page_header("02 · COLOR · DAY + ACCENT", "02")}
  <div class="kicker"><span>02</span><span class="dash">·</span><span>DAY · COURTESY</span></div>
  <h2 class="scene">DAY IS A<br><span class="accent">COURTESY.</span></h2>
  <p>
    Day exists because some surfaces insist on white backgrounds —
    invoices, press releases, partner decks. Accent shifts to ochre
    (<span class="mono">#A06800</span>) so the yellow stays
    brand-adjacent while meeting AA body contrast on cream.
  </p>
  <div style="margin-top:10pt;">
{day_html}
  </div>

  <div class="rule" style="margin-top:16pt;"></div>
  <h3>THE ACCENT IS EARNED.</h3>
  <p>
    Yellow (<span class="mono">#FFC400</span> on night,
    <span class="mono">#A06800</span> on day) is reserved for
    <em>three</em> things: the <span class="accent">.AI</span>
    suffix in the wordmark, the single key number a chart is about,
    and the one word a sentence is pointing to. A row-heading
    painted yellow because it "looks nice" devalues the stamp for
    the thing that actually deserves it.
  </p>
{page_footer(15, "CHAPTER 02 · COLOR")}
</div>''')

# --- P16 · USAGE EXAMPLES ---
pages.append(f'''<div class="page">
{page_header("02 · COLOR · USAGE", "02")}
  <div class="kicker"><span>02</span><span class="dash">·</span><span>USAGE</span></div>
  <h2 class="scene">ONE GOLD<br>PER <span class="accent">ROW.</span></h2>
  <p>
    The block below is a product dashboard fragment rendered only
    with the palette above — ink bg, paper text, mute labels, one
    gold stamp per row on the number the reader is meant to look
    at. No fills, no icons, no gradients.
  </p>

  <div class="infog">
    <div class="row"><span class="k">HYBRID QUERY P95</span><span class="v">38&nbsp;MS</span><span class="gold">GOLD</span></div>
    <div class="row"><span class="k">INGEST · SUSTAINED</span><span class="v">1.55&nbsp;M/S</span><span class="gold">GOLD</span></div>
    <div class="row"><span class="k">SIEM AGG · 1 M EVENTS</span><span class="v">0.4&nbsp;MS</span><span class="gold">GOLD</span></div>
    <div class="row"><span class="k">MEMORY · vs 4-NODE ES</span><span class="v">21×&nbsp;LESS</span><span class="gold">GOLD</span></div>
    <div class="row"><span class="k">BINARY · STATIC</span><span class="v">13&nbsp;MB</span><span class="gold">GOLD</span></div>
    <div class="row"><span class="k">COLD START · VS JVM</span><span class="v">300×&nbsp;FASTER</span><span class="gold">GOLD</span></div>
  </div>

  <p style="margin-top:10pt; color:var(--z-mute); font-size:9pt;">
    Six numbers, six gold stamps, six 1&thinsp;px dividers, zero
    decoration. Everything a reader needs to know about the product
    is already here.
  </p>
{page_footer(16, "CHAPTER 02 · COLOR")}
</div>''')

# --- P17 · EMPTY BREATHE PAGE? Skip - straight to Ch03 ---

# --- P17 · CHAPTER 03 DIVIDER ---
pages.append(f'''<div class="page" style="display:flex; flex-direction:column; justify-content:space-between;">
{page_header("CHAPTER 03 · TYPOGRAPHY", "03")}
  <div style="flex:1; display:flex; flex-direction:column; justify-content:center;">
    <h1 class="ch-num">03</h1>
    <h1 class="ch-title">TYPE.</h1>
    <p class="lead" style="max-width:5.6in; margin-top:6pt;">
      Four typefaces. Seven sizes. Nothing outside this grid ships.
      Type is the primary visual element, not a label next to a
      "real" graphic.
    </p>
  </div>
{page_footer(17, "CHAPTER 03 · TYPOGRAPHY")}
</div>''')

# --- P18 · TYPEFACES ---
pages.append(f'''<div class="page">
{page_header("03 · TYPOGRAPHY · TYPEFACES", "03")}
  <div class="kicker"><span>03</span><span class="dash">·</span><span>TYPEFACES</span></div>
  <h2 class="scene">FOUR <span class="accent">TYPEFACES.</span></h2>

  <div class="tf-row">
    <div class="tf-name">DISPLAY · HEADLINES</div>
    <div style="font-family:var(--font-display); font-weight:900; font-size:48pt; line-height:1; letter-spacing:-0.005em;">BIG SHOULDERS DISPLAY 900</div>
    <p style="margin-top:6pt; color:var(--z-mute); font-size:9pt;">A condensed display face. All caps. Used only in headlines, the wordmark, and hero moments. Never body copy.</p>
  </div>

  <div class="tf-row">
    <div class="tf-name">PROSE · BODY COPY</div>
    <div style="font-family:var(--font-prose); font-weight:400; font-size:18pt; line-height:1.3;">Inter Regular. The working voice of the system — long-form reading, captions, documentation.</div>
  </div>

  <div class="tf-row">
    <div class="tf-name">DATA · LABELS &amp; CHROME</div>
    <div style="font-family:var(--font-data); font-weight:600; font-size:13pt; letter-spacing:0.14em; text-transform:uppercase;">IBM PLEX SANS 600 · EYEBROWS · KICKERS · UI CHROME</div>
  </div>

  <div class="tf-row">
    <div class="tf-name">MONO · NUMBERS &amp; CODE</div>
    <div class="mono" style="font-size:13pt;">JetBrains Mono — identifiers, hex, paths, tabular numerics, p95=38ms</div>
  </div>
{page_footer(18, "CHAPTER 03 · TYPOGRAPHY")}
</div>''')

# --- P19 · SCALE LADDER ---
pages.append(f'''<div class="page">
{page_header("03 · TYPOGRAPHY · SCALE", "03")}
  <div class="kicker"><span>03</span><span class="dash">·</span><span>SCALE</span></div>
  <h2 class="scene">SEVEN <span class="accent">SIZES.</span></h2>
  <p>
    Everything is built from seven sizes. An eighth requires a
    500-word essay to the team defending the exception.
  </p>

  <div class="scale-row"><span class="px">11 PX</span><span style="font-size:11px; font-family:var(--font-data);">EYEBROW · KEY — "TYPE IS THE UI"</span><span class="use">Eyebrow · key</span></div>
  <div class="scale-row"><span class="px">13 PX</span><span style="font-size:13px;">Data · row · legend · 13&thinsp;px</span><span class="use">Data · row</span></div>
  <div class="scale-row"><span class="px">16 PX</span><span style="font-size:16px;">Body copy — reading text · 16&thinsp;px</span><span class="use">Body</span></div>
  <div class="scale-row"><span class="px">20 PX</span><span style="font-size:20px;">Lead paragraph · 20&thinsp;px</span><span class="use">Lead</span></div>
  <div class="scale-row"><span class="px">32 PX</span><span style="font-size:32px; font-family:var(--font-display); font-weight:900;">SECTION H4 · 32</span><span class="use">Section h4</span></div>
  <div class="scale-row"><span class="px">56 PX</span><span style="font-size:48px; font-family:var(--font-display); font-weight:900; line-height:1;">SECTION H3</span><span class="use">Section h3</span></div>
  <div class="scale-row"><span class="px">96 PX</span><span style="font-size:72px; font-family:var(--font-display); font-weight:900; line-height:1;">SCENE</span><span class="use">Scene h2</span></div>
{page_footer(19, "CHAPTER 03 · TYPOGRAPHY")}
</div>''')

# --- P20 · HIERARCHY / GRID ---
pages.append(f'''<div class="page">
{page_header("03 · TYPOGRAPHY · GRID", "03")}
  <div class="kicker"><span>03</span><span class="dash">·</span><span>GRID &amp; HIERARCHY</span></div>
  <h2 class="scene">12 COLUMNS.<br>24 PX <span class="accent">GUTTERS.</span></h2>

  <div class="swatch"><span style="font-family:var(--font-data); font-size:9pt; color:var(--z-mute); letter-spacing:var(--track-ui); text-transform:uppercase;">COLUMNS</span><span style="font-family:var(--font-prose); color:var(--z-ink);">12 — same on marketing, brand, product</span><span class="mono" style="color:var(--z-mute);">12</span></div>
  <div class="swatch"><span style="font-family:var(--font-data); font-size:9pt; color:var(--z-mute); letter-spacing:var(--track-ui); text-transform:uppercase;">GUTTER</span><span style="font-family:var(--font-prose); color:var(--z-ink);">24 px horizontal</span><span class="mono" style="color:var(--z-mute);">24 PX</span></div>
  <div class="swatch"><span style="font-family:var(--font-data); font-size:9pt; color:var(--z-mute); letter-spacing:var(--track-ui); text-transform:uppercase;">VERTICAL RHYTHM</span><span style="font-family:var(--font-prose); color:var(--z-ink);">4 · 8 · 12 · 16 · 24 · 32 · 48 · 64 · 96 · 128</span><span class="mono" style="color:var(--z-mute);">8-STEP</span></div>
  <div class="swatch"><span style="font-family:var(--font-data); font-size:9pt; color:var(--z-mute); letter-spacing:var(--track-ui); text-transform:uppercase;">MAX WIDTH</span><span style="font-family:var(--font-prose); color:var(--z-ink);">1680 px · centered above that</span><span class="mono" style="color:var(--z-mute);">1680</span></div>
  <div class="swatch"><span style="font-family:var(--font-data); font-size:9pt; color:var(--z-mute); letter-spacing:var(--track-ui); text-transform:uppercase;">BREAKPOINT</span><span style="font-family:var(--font-prose); color:var(--z-ink);">Single — 960 px</span><span class="mono" style="color:var(--z-mute);">960</span></div>

  <p style="margin-top:14pt; color:var(--z-mute); font-size:9pt;">
    There is no tablet layout. The product does not have a tablet
    user. Below 960&thinsp;px the hero folds to 96&thinsp;px and
    metrics stack two-up — everything else collapses naturally.
  </p>

  <div class="rule" style="margin-top:14pt;"></div>
  <h3>SCALE IS THE HIERARCHY.</h3>
  <p>
    Nothing bold, nothing italic, nothing larger than the next rung
    on the ladder. If a designer wants emphasis, they pick a bigger
    size — they do not bold the word. The scale carries meaning;
    weight is reserved for the typefaces themselves.
  </p>
{page_footer(20, "CHAPTER 03 · TYPOGRAPHY")}
</div>''')

# --- Filler P21 ---
# Actually skip and go to chapter 04 divider directly

# --- P21 · CHAPTER 04 DIVIDER ---
pages.append(f'''<div class="page" style="display:flex; flex-direction:column; justify-content:space-between;">
{page_header("CHAPTER 04 · UX & DATA", "04")}
  <div style="flex:1; display:flex; flex-direction:column; justify-content:center;">
    <h1 class="ch-num">04</h1>
    <h1 class="ch-title">UX &amp; DATA.</h1>
    <p class="lead" style="max-width:5.6in; margin-top:6pt;">
      Seventeen chart primitives. All 1&thinsp;px. An AI data
      company that cannot show a chart cleanly cannot sell storage
      cleanly either. Type is the infographic.
    </p>
  </div>
{page_footer(21, "CHAPTER 04 · UX & DATA")}
</div>''')

# Each chart gets a chart card on its own spread.
# Group: 2 charts per page where reasonable, 1 per page for wide ones.

# --- P22 · METRIC + SERIES ---
series_svg_html, s_mn, s_mx = series_svg(series_latency, w=1200, h=130)
pages.append(f'''<div class="page">
{page_header("04 · UX · PRIMITIVES 01-02", "04")}
  <div class="kicker"><span>04</span><span class="dash">·</span><span>CORE PRIMITIVES</span></div>
  <h2 class="scene">01 · METRIC.<br>02 · <span class="accent">SERIES.</span></h2>

  <h4>01 · METRIC · HEADLINE + 1PX SPARK</h4>
  <p>A number and its trend shape. No axis, no grid — the shape is the annotation.</p>
  <div class="chart-card">
    <div class="kicker"><span>INGEST · SUSTAINED</span></div>
    <div style="display:grid; grid-template-columns: auto 1fr auto; gap:16pt; align-items:baseline; margin-top:6pt; color:var(--z-ink);">
      <div style="font-family:var(--font-display); font-weight:900; font-size:60pt; line-height:0.9; color:var(--z-accent);">1.55<span style="font-family:var(--font-data); font-size:14pt; color:var(--z-mute); letter-spacing:var(--track-ui); margin-left:6pt;">M/S</span></div>
      <div style="max-width:3.4in; color:var(--z-ink);">{spark_svg(spark_throughput, w=260, h=40)}</div>
      <div style="text-align:right; font-family:var(--font-data); font-size:9pt; letter-spacing:var(--track-ui); text-transform:uppercase;">
        <span class="accent">▲ 3.1 %</span><br>
        <span style="color:var(--z-mute); font-size:8pt;">vs 24 H</span>
      </div>
    </div>
  </div>

  <h4 style="margin-top:18pt;">02 · SERIES · 1 PX TIME LINE</h4>
  <p>One stroke across the panel. Endpoints called out beneath, never floating over the curve.</p>
  <div class="chart-card">
    <div class="kicker"><span>HYBRID QUERY P95</span><span class="dash">·</span><span>48 H</span></div>
    <div style="margin-top:8pt;">{series_svg_html}</div>
    <div class="legend">
      <span>48 H AGO · <span class="mono">{series_latency[0]:.1f} ms</span></span>
      <span class="mid">min <span class="mono">{s_mn:.1f}</span> · peak <span class="mono">{s_mx:.1f}</span></span>
      <span><span class="mono accent">{series_latency[-1]:.1f} ms</span> · NOW</span>
    </div>
  </div>
{page_footer(22, "CHAPTER 04 · UX & DATA")}
</div>''')

# --- P23 · TOP-N + DIST ---
dist_svg_inline, dist_legend_inline = dist_svg_html(dist_segments)
pages.append(f'''<div class="page">
{page_header("04 · UX · PRIMITIVES 03-04", "04")}
  <div class="kicker"><span>04</span><span class="dash">·</span><span>CORE PRIMITIVES</span></div>
  <h2 class="scene">03 · TOP-N.<br>04 · <span class="accent">DIST.</span></h2>

  <h4>03 · TOP-N · RANKED WITH 1 PX BARS</h4>
  <p>Replaces the horizontal-bar chart. A ranked list <em>is</em> one — label, value, bar, share.</p>
  <div class="chart-card">
    <div class="kicker"><span>TOP ENDPOINTS</span><span class="dash">·</span><span>1 M REQUESTS · 24 H</span></div>
    <div class="topn-list" style="margin-top:6pt; color:var(--z-ink);">
{topn_rows_html(topn_items)}
    </div>
  </div>

  <h4 style="margin-top:18pt;">04 · DIST · 1 PX SEGMENTED BAR</h4>
  <p>Replaces pie and donut. Opacity carries the segment identity; lengths carry the share.</p>
  <div class="chart-card">
    <div class="kicker"><span>HTTP STATUS</span><span class="dash">·</span><span>24 H</span></div>
    <div style="margin-top:6pt; color:var(--z-ink);">
      {dist_svg_inline}
      <div class="dist-legend">
{dist_legend_inline}
      </div>
    </div>
  </div>
{page_footer(23, "CHAPTER 04 · UX & DATA")}
</div>''')

# --- P24 · HEATMAP + GAUGE ---
pages.append(f'''<div class="page">
{page_header("04 · UX · PRIMITIVES 05-06", "04")}
  <div class="kicker"><span>04</span><span class="dash">·</span><span>CORE PRIMITIVES</span></div>
  <h2 class="scene">05 · HEATMAP.<br>06 · <span class="accent">GAUGE.</span></h2>

  <h4>05 · HEATMAP · NUMBERS ARE THE VISUALISATION</h4>
  <p>A grid where text opacity encodes magnitude. No cells, no boxes, no colour bands.</p>
  <div class="chart-card">
    <div class="kicker"><span>AGENT ACTIVITY</span><span class="dash">·</span><span>COHORT × HOUR</span></div>
    <div style="margin-top:6pt; color:var(--z-ink);">
      <div class="heatmap">{heatmap_html(heat_rows_labels, heat_cols, heat_matrix)}</div>
    </div>
  </div>

  <h4 style="margin-top:18pt;">06 · GAUGE · 1 PX TRACK</h4>
  <p>A bounded track that fills from zero to the reading. The threshold is a single 1&thinsp;px tick.</p>
  <div class="chart-card">
    <div class="kicker"><span>DISK PRESSURE · WAL VOLUME</span></div>
    <div style="margin-top:6pt; color:var(--z-ink);">{gauge_html(73)}</div>
  </div>
{page_footer(24, "CHAPTER 04 · UX & DATA")}
</div>''')

# --- P25 · MULTIPLES + BAR ---
bar_svg_html, bar_mn, bar_mx = bar_html()
pages.append(f'''<div class="page">
{page_header("04 · UX · PRIMITIVES 07-08", "04")}
  <div class="kicker"><span>04</span><span class="dash">·</span><span>EXPANDED</span></div>
  <h2 class="scene">07 · MULTIPLES.<br>08 · <span class="accent">BAR.</span></h2>

  <h4>07 · MULTIPLES · EIGHT SPARKS</h4>
  <p>One sparkline per dimension. The reader scans for the one that broke ranks.</p>
  <div class="chart-card">
    <div class="kicker"><span>SERVICE LATENCIES</span><span class="dash">·</span><span>P95 · 24 H</span></div>
    <div style="margin-top:6pt; color:var(--z-ink);">{multiples_html()}</div>
  </div>

  <h4 style="margin-top:18pt;">08 · BAR · VERTICAL 1 PX LINES</h4>
  <p>A bar is a stroke from baseline to value. No fill, no cap, no outline.</p>
  <div class="chart-card">
    <div class="kicker"><span>QUERIES BY HOUR</span><span class="dash">·</span><span>LAST 24 H</span></div>
    <div style="margin-top:6pt; color:var(--z-ink);">{bar_svg_html}</div>
  </div>
{page_footer(25, "CHAPTER 04 · UX & DATA")}
</div>''')

# --- P26 · SCATTER + STACKED ---
pages.append(f'''<div class="page">
{page_header("04 · UX · PRIMITIVES 09-10", "04")}
  <div class="kicker"><span>04</span><span class="dash">·</span><span>EXPANDED</span></div>
  <h2 class="scene">09 · SCATTER.<br>10 · <span class="accent">STACKED.</span></h2>

  <h4>09 · SCATTER · DOTS ARE CHARACTERS</h4>
  <p>The only chart with "point markers" — because the <span class="mono">·</span> <em>is</em> the datum.</p>
  <div class="chart-card">
    <div class="kicker"><span>LATENCY × THROUGHPUT</span><span class="dash">·</span><span>80 TENANTS</span></div>
    <div style="margin-top:6pt; color:var(--z-ink);">{scatter_html()}</div>
  </div>

  <h4 style="margin-top:18pt;">10 · STACKED · HORIZONTAL DISTRIBUTIONS</h4>
  <p>One <span class="mono">DIST</span> per row. Opacity carries the segment; lengths carry share.</p>
  <div class="chart-card">
    <div class="kicker"><span>HTTP STATUS BY REGION</span></div>
    <div style="margin-top:6pt; color:var(--z-ink);">{stacked_html()}</div>
  </div>
{page_footer(26, "CHAPTER 04 · UX & DATA")}
</div>''')

# --- P27 · TREEMAP + EVENTS ---
pages.append(f'''<div class="page">
{page_header("04 · UX · PRIMITIVES 11-12", "04")}
  <div class="kicker"><span>04</span><span class="dash">·</span><span>EXPANDED</span></div>
  <h2 class="scene">11 · TREEMAP.<br>12 · <span class="accent">EVENTS.</span></h2>

  <h4>11 · TREEMAP · INDENTED HIERARCHY</h4>
  <p>Not D3 — no packed rectangles. An indented ranked list with 1&thinsp;px bars, one row per node.</p>
  <div class="chart-card">
    <div class="kicker"><span>REQUEST TREE</span><span class="dash">·</span><span>API GATEWAY</span></div>
    <div style="margin-top:6pt; color:var(--z-ink);">{treemap_html()}</div>
  </div>

  <h4 style="margin-top:18pt;">12 · EVENTS · LIVE TAIL</h4>
  <p>Time, severity, message. Errors take the gold accent; info stays mute.</p>
  <div class="chart-card">
    <div class="kicker"><span>LIVE TAIL</span><span class="dash">·</span><span>TENANT=acme</span></div>
    <div style="margin-top:6pt; color:var(--z-ink);">{events_html()}</div>
  </div>
{page_footer(27, "CHAPTER 04 · UX & DATA")}
</div>''')

# --- P28 · CHAPTER 04 SUB-HEAD · AI primitives intro ---
pages.append(f'''<div class="page">
{page_header("04 · UX · AI PRIMITIVES", "04")}
  <div class="kicker"><span>04</span><span class="dash">·</span><span>AI · RAG · AGENTS</span></div>
  <h2 class="scene">SAME LINE.<br>SAME <span class="accent">RULE.</span></h2>
  <p class="lead">
    The charts that follow are specific to AI workloads — embedding
    clusters, retrieval flows, token budgets, multi-dim agent runs,
    attention. They are not a different language. The strokes are
    still 1&thinsp;px, the type is still the label, the accent still
    falls only where the reader is meant to look.
  </p>
  <div class="rule"></div>
  <div class="kicker"><span>CHARTS · 13–17</span></div>
  <ul style="padding-left:0; list-style:none; margin-top:10pt;">
    <li style="padding:6pt 0; border-bottom:1px solid var(--z-faint); font-family:var(--font-data); font-size:10pt; letter-spacing:var(--track-ui); text-transform:uppercase; color:var(--z-ink);">13 · EMBEDSPACE — clusters · UMAP projection · 1 px convex hulls</li>
    <li style="padding:6pt 0; border-bottom:1px solid var(--z-faint); font-family:var(--font-data); font-size:10pt; letter-spacing:var(--track-ui); text-transform:uppercase; color:var(--z-ink);">14 · CHORDARCS — connectivity · Bézier arcs · source → target</li>
    <li style="padding:6pt 0; border-bottom:1px solid var(--z-faint); font-family:var(--font-data); font-size:10pt; letter-spacing:var(--track-ui); text-transform:uppercase; color:var(--z-ink);">15 · FLOWBAND — allocation · one horizontal strip · labelled</li>
    <li style="padding:6pt 0; border-bottom:1px solid var(--z-faint); font-family:var(--font-data); font-size:10pt; letter-spacing:var(--track-ui); text-transform:uppercase; color:var(--z-ink);">16 · PARALLELCOORDS — multi-dim · N axes · 1 px per row</li>
    <li style="padding:6pt 0; font-family:var(--font-data); font-size:10pt; letter-spacing:var(--track-ui); text-transform:uppercase; color:var(--z-ink);">17 · ATTENTIONMAP — tokens · per-token opacity · paragraph is the chart</li>
  </ul>
{page_footer(28, "CHAPTER 04 · UX & DATA")}
</div>''')

# --- P29 · EMBEDSPACE ---
pages.append(f'''<div class="page">
{page_header("04 · UX · 13 EMBEDSPACE", "04")}
  <div class="kicker"><span>13</span><span class="dash">·</span><span>EMBEDSPACE · CLUSTERS</span></div>
  <h2 class="scene">CLUSTERS.<br>WITH A <span class="accent">QUERY.</span></h2>
  <p>
    The iconic AI visualisation. 2&thinsp;D UMAP projection of
    high-dimensional embeddings, with 1&thinsp;px convex hulls
    around each cluster and centroid labels sitting outside the
    cloud. Dots are <span class="mono">·</span> characters; the
    gold crosshair is the current query's projected embedding. No
    kmeans fuzz, no colour palette.
  </p>

  <div class="chart-card">
    <div class="kicker"><span>EMBEDDING SPACE</span><span class="dash">·</span><span>UMAP</span></div>
    <div style="margin-top:6pt; color:var(--z-ink);">{embedspace_html()}</div>
  </div>
{page_footer(29, "CHAPTER 04 · UX & DATA")}
</div>''')

# --- P30 · CHORDARCS ---
pages.append(f'''<div class="page">
{page_header("04 · UX · 14 CHORDARCS", "04")}
  <div class="kicker"><span>14</span><span class="dash">·</span><span>CHORDARCS · CONNECTIVITY</span></div>
  <h2 class="scene">RETRIEVAL.<br>AS <span class="accent">ARCS.</span></h2>
  <p>
    Flow between two ordered sets — queries on the left, retrieved
    chunks on the right, one 1&thinsp;px Bézier arc per retrieval.
    Arc opacity is the relevance score. Replaces Sankey for RAG
    pipelines because a Sankey wastes ninety percent of its ink on
    colour-coded ribbons; we only need the line.
  </p>

  <div class="chart-card">
    <div class="kicker"><span>RAG RETRIEVAL</span><span class="dash">·</span><span>TOP-K=2</span></div>
    <div style="margin-top:6pt; color:var(--z-ink);">{chordarcs_html()}</div>
  </div>
{page_footer(30, "CHAPTER 04 · UX & DATA")}
</div>''')

# --- P31 · FLOWBAND + PARALLELCOORDS ---
pages.append(f'''<div class="page">
{page_header("04 · UX · 15-16", "04")}
  <div class="kicker"><span>04</span><span class="dash">·</span><span>AI · 15-16</span></div>
  <h2 class="scene">15 · FLOWBAND.<br>16 · <span class="accent">PARALLELCOORDS.</span></h2>

  <h4>15 · FLOWBAND · ALLOCATION</h4>
  <p>Horizontal strip split into segments, each labelled directly beneath. Token budgets, context windows, cost attribution.</p>
  <div class="chart-card">
    <div class="kicker"><span>CONTEXT WINDOW</span><span class="dash">·</span><span>28 K TOKENS</span></div>
    <div style="margin-top:6pt; color:var(--z-ink);">{flowband_html()}</div>
  </div>

  <h4 style="margin-top:18pt;">16 · PARALLELCOORDS</h4>
  <p>N axes, N dimensions, one 1&thinsp;px polyline per row. Gold line = one highlighted run in the density cloud.</p>
  <div class="chart-card">
    <div class="kicker"><span>AGENT RUNS</span><span class="dash">·</span><span>24 ROWS · 5 DIMENSIONS</span></div>
    <div style="margin-top:6pt; color:var(--z-ink);">{parallelcoords_html()}</div>
  </div>
{page_footer(31, "CHAPTER 04 · UX & DATA")}
</div>''')

# --- P32 · ATTENTIONMAP ---
pages.append(f'''<div class="page">
{page_header("04 · UX · 17 ATTENTIONMAP", "04")}
  <div class="kicker"><span>17</span><span class="dash">·</span><span>ATTENTIONMAP</span></div>
  <h2 class="scene">THE PARAGRAPH<br>IS THE <span class="accent">CHART.</span></h2>
  <p>
    Every token's opacity equals its attention weight; the peak
    tokens cross into the gold accent. For a RAG system this is the
    cleanest possible answer to <em>which tokens actually
    mattered</em>. No heatmap overlay, no coloured highlights —
    type rendered at the weight the model assigned.
  </p>

  <div class="chart-card" style="padding:20pt 0;">
    <div class="kicker"><span>ATTENTION · ANSWER TOKENS</span><span class="dash">·</span><span>PROMPT="WHAT DOES XERJ DO?"</span></div>
    <div style="margin-top:14pt;">{attentionmap_html()}</div>
  </div>

  <div class="rule" style="margin-top:18pt;"></div>
  <h3>17 PRIMITIVES. ONE RULE.</h3>
  <p>
    Every chart in this chapter is a 1&thinsp;px stroke, a number,
    or an opacity applied to a character. No fills, no colour bars,
    no icons on axes, no legends floating over curves. The storage
    is <em>for</em> the chart; the chart <em>is</em> the storage;
    the rules are the same line.
  </p>
{page_footer(32, "CHAPTER 04 · UX & DATA")}
</div>''')

# --- P33 · CHAPTER 05 DIVIDER ---
pages.append(f'''<div class="page" style="display:flex; flex-direction:column; justify-content:space-between;">
{page_header("CHAPTER 05 · PHILOSOPHY", "05")}
  <div style="flex:1; display:flex; flex-direction:column; justify-content:center;">
    <h1 class="ch-num">05</h1>
    <h1 class="ch-title">PHILOSOPHY.</h1>
    <p class="lead" style="max-width:5.6in; margin-top:6pt;">
      Why the brand looks like this. Three decisions, none
      decorative — a gold accent, a 1&thinsp;px stroke, typography
      doing the work a chart would do.
    </p>
  </div>
{page_footer(33, "CHAPTER 05 · PHILOSOPHY")}
</div>''')

# --- P34 · GOLD ---
pages.append(f'''<div class="page">
{page_header("05 · PHILOSOPHY · GOLD", "05")}
  <div class="kicker"><span>05</span><span class="dash">·</span><span>GOLD</span></div>
  <h2 class="scene">THE GOLD IS<br>A <span class="accent">STANDARD.</span></h2>

  <p>
    Every industry that has a reference calls it the <span class="accent">gold standard</span>.
    A gold bar is 99.99&thinsp;% pure by defined assay. A gold medal
    is the winner. A gold seal is notarised truth. The word holds
    across centuries — the Roman <em>solidus</em>, the Arabic
    <em>dinar</em>, gold leaf on Qur'anic manuscripts, gold trim on
    a <em>thobe</em>, gold on any document that has to still mean
    something ten years later. Gold earned the name because gold
    does not tarnish.
  </p>

  <p>
    XERJ takes its name from the Arabic word for life —
    <span class="accent">ḥayāh</span>&nbsp;(حياة). In that
    tradition, gold carries weight beyond wealth. A gift of gold
    marks a milestone. Gold thread marks craft meant to outlast
    the wearer. Gold ink on parchment marks a truth the writer
    was willing to sign. Across all of it, gold is applied only to
    things already good enough to carry its weight.
    <em>Success, not material.</em>
  </p>

  <p>
    We use the accent (<span class="mono">#FFC400</span>) the same
    way. It marks the <em>one</em> number a chart is about, the
    <em>one</em> word a sentence is pointing to, the p95 that says
    we are shipping. If a designer paints a row heading yellow
    because it "looks nice", they have devalued the stamp for the
    thing that actually deserves it.
  </p>
{page_footer(34, "CHAPTER 05 · PHILOSOPHY")}
</div>''')

# --- P35 · 1PX ---
pages.append(f'''<div class="page">
{page_header("05 · PHILOSOPHY · 1 PX", "05")}
  <div class="kicker"><span>05</span><span class="dash">·</span><span>1 PX</span></div>
  <h2 class="scene">1 PX IS THE<br>MINIMUM THAT<br><span class="accent">CARRIES MEANING.</span></h2>

  <p>
    Any modern screen can render a 1&thinsp;px line. Most brands do
    not trust that to be enough. They add a gradient to give it
    depth. They add a shadow to give it weight. They widen it to
    2&thinsp;px "for emphasis". Each addition is a concession that
    the idea underneath was not strong enough to carry the line
    alone.
  </p>

  <p>
    Our rule is absolute. Every hairline, every axis tick, every
    divider, every border, every bar in every chart:
    <span class="accent">one pixel</span>. Never wider. Never
    gradient. Never feathered. If a design needs a thicker stroke
    to be legible, the design has a bigger problem that the
    thicker stroke is covering up. Fix the idea, not the border.
  </p>

  <p>
    This is engineering discipline, not aesthetic minimalism.
    Engineers do not ship a thousand lines of code when ten will
    do. Designers who have lived inside production systems do not
    ship 3&thinsp;px borders when 1&thinsp;px is sufficient. The
    narrowest line that still carries the information is the
    correct line.
  </p>
{page_footer(35, "CHAPTER 05 · PHILOSOPHY")}
</div>''')

# --- P36 · TYPE AS INFOGRAPHIC ---
pages.append(f'''<div class="page">
{page_header("05 · PHILOSOPHY · TYPE", "05")}
  <div class="kicker"><span>05</span><span class="dash">·</span><span>TYPE IS DATA</span></div>
  <h2 class="scene">TYPE IS THE<br><span class="accent">INFOGRAPHIC.</span></h2>

  <p>
    An AI system outputs text. A log line is text. A query plan is
    text. A vector is a sequence of numbers rendered as text. A
    retrieval result is a snippet of text ranked against other
    snippets of text. The entire observable surface of a language
    model — prompt, response, embedding, audit trail — is plain
    text.
  </p>

  <p>
    Marketing designers usually translate that text into pictures:
    illustrations, flowcharts, isometric servers with tiny humans
    standing next to them. Every translation loses fidelity. A
    query result drawn as a generic "document" icon tells you
    nothing; the same result as monospaced JSON tells you
    everything.
  </p>

  <p>
    We do the opposite. Our dashboards are tables with good
    typographic taste. Our charts are 1&thinsp;px strokes over a
    typographic axis. Our marketing pages render the same numbers
    our customers see on their consoles, at the same precision,
    without stylisation. Type is the native format of an AI system.
    Any other format is a translation <em>away</em> from the truth.
    Typography is not a substitute for an infographic —
    <span class="accent">it is the infographic</span>.
  </p>
{page_footer(36, "CHAPTER 05 · PHILOSOPHY")}
</div>''')

# --- P37 · CHAPTER 06 DIVIDER ---
pages.append(f'''<div class="page" style="display:flex; flex-direction:column; justify-content:space-between;">
{page_header("CHAPTER 06 · VOICE", "06")}
  <div style="flex:1; display:flex; flex-direction:column; justify-content:center;">
    <h1 class="ch-num">06</h1>
    <h1 class="ch-title">VOICE.</h1>
    <p class="lead" style="max-width:5.6in; margin-top:6pt;">
      Solid. Quiet. Technical. We write for engineers paged at
      2&nbsp;am. Every sentence earns its place.
    </p>
  </div>
{page_footer(37, "CHAPTER 06 · VOICE")}
</div>''')

# --- P38 · VOICE DO/DONT/NEVER ---
pages.append(f'''<div class="page">
{page_header("06 · VOICE", "06")}
  <div class="kicker"><span>06</span><span class="dash">·</span><span>DO · DON'T · NEVER</span></div>
  <h2 class="scene">SOLID.<br>QUIET.<br><span class="accent">TECHNICAL.</span></h2>

  <div class="verdict"><span class="tag yes">DO</span><span class="body">"Hybrid query p95 is 38&thinsp;ms at 1&thinsp;B vectors on one box." · "One binary, no JVM heap tuning." · "Agents are the workload." · "Explain plan is first-class."</span></div>
  <div class="verdict"><span class="tag no">DON'T</span><span class="body">"Revolutionary." · "Paradigm shift." · "Unleash the power of..." · "Next-generation AI-powered cloud-native..." · Anything with <em>journey</em>, <em>storytelling</em>, or <em>magic</em>.</span></div>
  <div class="verdict"><span class="tag no">NEVER</span><span class="body">Emojis in marketing. Humour as the primary register. Cute mascots. "Fun" at the expense of precision. We ship storage. Storage is serious.</span></div>

  <div class="rule"></div>
  <h3>LEAD WITH A NUMBER, A CONSTRAINT, OR A NAME.</h3>
  <p>
    Copy is not a pitch deck. Every opening sentence should contain
    at least one of: a measured value, a hard constraint, or a
    specific system name. If a line could appear in a LinkedIn
    influencer post, it does not ship.
  </p>

  <h3>BE <span class="accent">DIRECT</span>, NOT WARM.</h3>
  <p>
    We are not selling comfort. Warm copy reads as slick in our
    context. An engineer reading at 2&nbsp;am does not want to be
    charmed; they want to know what the p95 is and whether it can
    take another tenant before the budget breaks. Answer that
    question, end the sentence, move on.
  </p>
{page_footer(38, "CHAPTER 06 · VOICE")}
</div>''')

# --- P39 · CHAPTER 07 DIVIDER ---
pages.append(f'''<div class="page" style="display:flex; flex-direction:column; justify-content:space-between;">
{page_header("CHAPTER 07 · RESOURCES", "07")}
  <div style="flex:1; display:flex; flex-direction:column; justify-content:center;">
    <h1 class="ch-num">07</h1>
    <h1 class="ch-title">RESOURCES.</h1>
    <p class="lead" style="max-width:5.6in; margin-top:6pt;">
      SVG downloads, cross-references, and links to the interactive
      playground where the chart primitives render live.
    </p>
  </div>
{page_footer(39, "CHAPTER 07 · RESOURCES")}
</div>''')

# --- P40 · DOWNLOADS ---
dls = [
    ('WORDMARK · NIGHT',        'Primary lockup. Paper on night, yellow .AI. Use on every dark surface.',                 '/brandbook/xerj-wordmark-night.svg'),
    ('WORDMARK · DAY',          'Courtesy treatment. Ink on cream, ochre .AI for AA contrast on paper.',                  '/brandbook/xerj-wordmark-day.svg'),
    ('WORDMARK · MONO',         'currentColor fill for emails, third-party decks, unknown backgrounds.',                   '/brandbook/xerj-wordmark-mono.svg'),
    ('SHORT FORM',              'XERJ without .AI. App title bars, compact headers, system tray.',                        '/brandbook/xerj-short.svg'),
    ('LETTER MARK',             'Yellow X on night. Favicon, app icon, social avatar.',                                    '/brandbook/xerj-mark.svg'),
    ('LETTER MARK · INVERSE',   'Ink X on paper. Letterhead, print covers, debossed surfaces.',                            '/brandbook/xerj-mark-inverse.svg'),
    ('SYSTEM LINE',             'XERJ.AI · OBSERVE tracked signature for footers and headers.',                           '/brandbook/xerj-systemline.svg'),
]
dl_html = "\n".join(f'''  <div class="dl-row">
    <span class="name">{n}</span>
    <span class="desc">{d}</span>
    <span class="get">SVG →</span>
  </div>''' for n, d, _ in dls)

pages.append(f'''<div class="page">
{page_header("07 · RESOURCES · DOWNLOADS", "07")}
  <div class="kicker"><span>07</span><span class="dash">·</span><span>DOWNLOADS</span></div>
  <h2 class="scene">VECTORS.<br><span class="accent">NO RASTERS.</span></h2>
  <p>
    Every asset ships as SVG. No PNG, no JPG variants — a raster of
    the wordmark is either wrong size or wrong compression, and both
    of those are easier to fix upstream by using the SVG.
  </p>
  <div class="dl-list" style="margin-top:10pt;">
{dl_html}
  </div>

  <div class="rule"></div>
  <h3>CROSS-REFERENCES.</h3>
  <div class="dl-list">
    <div class="dl-row"><span class="name">BRAND CONCEPT</span><span class="desc">/brand.html · the original five laws and voice spec.</span><span class="get">URL →</span></div>
    <div class="dl-row"><span class="name">ARCHITECTURE</span><span class="desc">/architecture/index.html · 1 px SVG topologies of the platform.</span><span class="get">URL →</span></div>
    <div class="dl-row"><span class="name">PLAYGROUND</span><span class="desc">/playground.html · every chart primitive in this chapter, live.</span><span class="get">URL →</span></div>
  </div>
{page_footer(40, "CHAPTER 07 · RESOURCES")}
</div>''')

# --- P41 · BACK COVER ---
pages.append(f'''<div class="page" style="padding:0.7in; display:flex; flex-direction:column; justify-content:space-between;">
  <div>
    <div class="kicker big"><span>XERJ.AI</span><span class="dash">·</span><span>END OF BRAND BOOK</span></div>
  </div>

  <div style="display:flex; flex-direction:column; justify-content:center; gap:28pt; padding:20pt 0;">
    <div style="width:100%; max-width:5.2in;">
      {wordmark_svg()}
    </div>

    <p style="font-family: var(--font-display); font-weight:900; font-size: 34pt; line-height: 0.95; letter-spacing: -0.005em; max-width: 6in; margin:0;">
      TYPE IS THE UI.<br>
      <span class="accent">1 PX IS THE STANDARD.</span><br>
      GOLD IS <span class="accent">EARNED</span>.
    </p>

    <p style="color: var(--z-mute); font-size: 10pt; font-family: var(--font-data); letter-spacing: var(--track-ui); text-transform: uppercase;">
      7 CHAPTERS · 17 CHART PRIMITIVES · 14 DO/DON'T RULES · 5 COLOURS · 4 TYPEFACES · 7 DOWNLOADS
    </p>
  </div>

  <div style="display:flex; justify-content:space-between; font-family:var(--font-data); font-size:9pt; letter-spacing:var(--track-ui); text-transform:uppercase; color:var(--z-mute); padding-top:10pt; border-top:1px solid var(--z-line);">
    <span>XERJ.AI · OBSERVE</span>
    <span>BRAND BOOK · V1 · 2026-04</span>
  </div>
</div>''')

# ============================================================
# WRITE OUT
# ============================================================

html = f'''<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>XERJ.AI — Brand Book · V1</title>
<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Big+Shoulders+Display:wght@400;700;800;900&family=Inter:wght@400;500;600;700&family=IBM+Plex+Sans:wght@400;500;600;700&family=JetBrains+Mono:wght@400;500;700&display=swap" rel="stylesheet">
<style>{STYLE}</style>
</head>
<body>
{chr(10).join(pages)}
</body>
</html>
'''

out = Path('/home/claude/ai/xerj.ai/brief/brandbook-pdf/brandbook.html')
out.write_text(html)
print(f"wrote {out} · {len(html):,} bytes · {len(pages)} pages")
