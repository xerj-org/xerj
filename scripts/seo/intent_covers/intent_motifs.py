"""Dot motifs for the agent-intent answer covers (landing/og/answers-<slug>.png).

Same drawing rules as the blog series (scripts/seo/blog_covers/motifs.py, whose
primitives these reuse): dots, hairlines and mono labels in the series inks.
Every number a motif draws is a measured value printed on the answer page it
illustrates (runs of 2026-10-19 on v1.0.0-rc.97); the docstring names it.
``rng`` only jitters placement and is seeded from the slug.
"""

from __future__ import annotations

import math
import pathlib
import random
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "blog_covers"))
from motifs import dot, dotbar, dotpath, hair, label, ring, bezier  # noqa: E402


def m_bytes(w: float, h: float, rng: random.Random) -> str:
    """local-code-search-mcp-fewer-tokens-than-grep: bytes an agent reads for
    one question on psf/requests. rg -n -i redirect: 25,150 B over 17 files;
    xerj search -k 3 --full 400: 1,303 B; xerj def: 949 B. Bar length to scale."""
    out = []
    full = w * 0.92
    pitch = 7.0
    rows = [("RG -N -I REDIRECT", 25150, "25,150 B · 282 LINES", "b"),
            ("XERJ SEARCH -K 3", 1303, "1,303 B · SESSIONS.PY:309", "y"),
            ("XERJ DEF", 949, "949 B · SESSIONS.PY:154", "y")]
    y = 20.0
    for name, n, note, ink in rows:
        out.append(label(0, y, name, "lab w" if ink == "y" else "lab"))
        length = max(pitch * 2, full * n / 25150)
        out.append(dotbar(0, y + 12, length, 5, pitch, ink, 4 if ink == "b" else 1))
        out.append(label(length + 12 if ink == "y" else 0, y + 12 + 5 * pitch + (-14 if ink == "y" else 18),
                         note, "num y" if ink == "y" else "num b"))
        y += 128 if ink == "b" else 104
    out.append(label(w, h - 6, "19× FEWER BYTES", "tag y", "end"))
    return "".join(out)


def m_fields(w: float, h: float, rng: random.Random) -> str:
    """extract-structured-data-from-web-page-json-schema: five claimed fields
    checked against saved pages; four found with a source locator (#s0, #s2,
    #s1, #s3), one ("14-day free trial") not found."""
    out = []
    rows = [("PRICING_UNIT", "PRICING#S0", True), ("USAGE_FEES", "PRICING#S2", True),
            ("COMPLIANCE_ADDON", "PRICING#S1", True), ("FREE_TRIAL", "NOT FOUND", False),
            ("TELEMETRY", "SECURITY#S3", True)]
    gap = h / 5.3
    y = 26.0
    for name, src, ok in rows:
        out.append(label(0, y, name, "lab w"))
        x0 = 0.0
        pitch = 8.0
        for i in range(int(w * 0.62 // pitch)):
            r = pitch * 0.42 * (1 + rng.uniform(-0.08, 0.08))
            out.append(dot(x0 + (i + 0.5) * pitch, y + 16, r * (0.55 if not ok else 1),
                           "y" if ok else "f"))
        end = w * 0.62 + 18
        if ok:
            out.append(dot(end + 8, y + 16, 7, "y"))
            out.append(label(end + 24, y + 21, src, "num y"))
        else:
            out.append(ring(end + 8, y + 16, 7, "m", 1.6))
            out.append(label(end + 24, y + 21, src, "num m"))
        y += gap
    return "".join(out)


def m_rows(w: float, h: float, rng: random.Random) -> str:
    """full-text-search-over-jsonl-files-cli: the 1,000-row agent_queries.csv as a
    dot field; 79 rows match "bm25 jsonl folder" (took 1 ms) and the top hit,
    id 480, is the one yellow dot. An exact rg phrase search found 0."""
    out = []
    cols, rows = 40, 25
    pitch = min(w / cols, (h - 60) / rows)
    hits = set(rng.sample(range(1000), 79))
    top = 479
    hits.add(top)
    for k in range(1000):
        j, i = divmod(k, cols)
        x, y = (i + 0.5) * pitch, 10 + (j + 0.5) * pitch
        if k == top:
            out.append(dot(x, y, pitch * 0.62, "y"))
            out.append(ring(x, y, pitch * 1.25, "y", 1.6))
        elif k in hits:
            out.append(dot(x, y, pitch * 0.38, "w"))
        else:
            out.append(dot(x, y, pitch * 0.16, "m"))
    yb = 10 + rows * pitch + 30
    out.append(label(0, yb, "1,000 CSV ROWS · 79 BM25 HITS · 1 MS", "lab w"))
    out.append(label(0, yb + 20, "ID 480 RANKED FIRST · RG PHRASE: 0", "lab y"))
    return "".join(out)


def m_ask(w: float, h: float, rng: random.Random) -> str:
    """ask-questions-about-a-github-repo-without-cloning: a question routes to
    one of the 14 corpora in the signed catalogue (snappy), whose 10.8 MB pack
    lands on a local node on port 9300. Cold 2,356 ms, warm 30 to 40 ms;
    answer snappy/snappy.cc:1643."""
    out = []
    names = ["act", "age", "agentops", "aiohttp", "alertmanager", "arkworks", "arroyo",
             "astro", "attack-map", "autorag", "avro", "lz4", "snappy", "zstd"]
    tw, th, gx = 118, 36, 12
    cols = 4
    x0 = w - cols * tw - (cols - 1) * gx
    for n, name in enumerate(names):
        r, c = divmod(n, cols)
        x, y = x0 + c * (tw + gx), 8 + r * (th + gx)
        hot = name == "snappy"
        pitch = 6.0
        for j in range(int(th // pitch)):
            for i in range(int(tw // pitch)):
                out.append(dot(x + (i + .5) * pitch, y + (j + .5) * pitch,
                               pitch * (0.44 if hot else 0.16), "y" if hot else "f"))
        out.append(label(x + 6, y + th - 8, name.upper(), "tag w" if hot else "tag m"))
        if hot:
            hx, hy = x + tw / 2, y + th
    qy = h - 70
    out.append(dotpath([(hx, hy + 8), (hx, qy + 4)], 9, 3.4, 3.4, "y"))
    out.append(label(hx + 26, qy - 46, '"HOW DOES SNAPPY', "lab w"))
    out.append(label(hx + 26, qy - 28, ' DECOMPRESS A LITERAL"', "lab w"))
    out.append(dotpath(bezier((hx + 120, qy - 64), (hx + 120, hy + 30), (hx + 40, hy + 6)), 9, 1.6, 3.0, "y"))
    out.append(dot(hx, qy + 16, 12, "y"))
    out.append(label(hx + 22, qy + 21, "NODE :9300 · SNAPPY.CC:1643", "num y"))
    out.append(label(0, h - 2, "COLD 2,356 MS · WARM 30–40 MS", "lab"))
    return "".join(out)


def m_batch(w: float, h: float, rng: random.Random) -> str:
    """batch-lookup-cve-ghsa-ids-offline: six IDs in one _search over the signed
    rust-vulns pack (2,033 advisories); four resolve to a record, two abstain.
    Wall time 15 ms."""
    out = []
    ids = [("CVE-2021-25900", "RUSTSEC-2021-0003", True), ("GHSA-9Q5J-JM53-V7VR", "RUSTSEC-2022-0051", True),
           ("CVE-2026-35355", "GHSA-239G-2685-54X3", True), ("RUSTSEC-2023-0071", "RSA", True),
           ("CVE-2024-24576", "ABSTAIN", False), ("CVE-2099-00001", "ABSTAIN", False)]
    hubx, huby = w * 0.42, h * 0.46
    gap = h / 6.6
    for n, (q, a, ok) in enumerate(ids):
        y = 18 + n * gap
        out.append(label(0, y + 4, q, "tag w"))
        out.append(dotpath(bezier((150, y), (hubx - 40, y), (hubx, huby)), 8, 1.4, 2.6, "w" if ok else "m"))
        out.append(dotpath(bezier((hubx, huby), (hubx + 50, y), (w * 0.62, y)), 8, 2.6, 1.6 if ok else 1.2,
                           "y" if ok else "m"))
        if ok:
            out.append(dot(w * 0.62 + 8, y, 6, "y"))
            out.append(label(w * 0.62 + 20, y + 4, a, "tag y"))
        else:
            out.append(ring(w * 0.62 + 8, y, 6, "m", 1.6))
            out.append(label(w * 0.62 + 20, y + 4, a, "tag m"))
    out.append(dot(hubx, huby, 16, "y"))
    out.append(label(hubx, huby + 36, "1 QUERY · 15 MS", "num y", "middle"))
    out.append(label(0, h - 2, "SIGNED PACK · 2,033 ADVISORIES · NO GITHUB API", "lab"))
    return "".join(out)


def m_abstain(w: float, h: float, rng: random.Random) -> str:
    """retrieval-that-says-not-found-with-file-line-citations: three outcomes
    from xerj ask. An exact definition answers with file:line (zstd.h:134);
    an unknown symbol prints "no match" and exits 1; an off-topic question to
    a pinned corpus still returns passages with exit 0 (no calibrated score
    yet, issue #1786)."""
    out = []
    cy = h * 0.42
    xs = [w * 0.16, w * 0.50, w * 0.84]
    R = 66
    # answered: full yellow disc
    for j in range(-9, 10):
        for i in range(-9, 10):
            x, y = xs[0] + i * 7.4, cy + j * 7.4
            d = math.hypot(x - xs[0], y - cy) / R
            if d <= 1:
                out.append(dot(x, y, 3.4 * math.sqrt(max(0.05, 1 - d * d)), "y"))
    # no match: an empty ring of dots
    for k in range(48):
        a = 2 * math.pi * k / 48
        out.append(dot(xs[1] + R * math.cos(a), cy + R * math.sin(a), 2.6, "m"))
    # off-topic: a blue scatter that should have abstained
    for _ in range(90):
        a, r = rng.uniform(0, 2 * math.pi), R * math.sqrt(rng.random())
        out.append(dot(xs[2] + r * math.cos(a), cy + r * math.sin(a), rng.uniform(1.2, 3.2), "b"))
    rows = [("ANSWERED", "ZSTD.H:134 · EXIT 0", "y"), ("NO MATCH", "EXIT 1 · 20 MS", "w"),
            ("OFF-TOPIC", "EXIT 0 · #1786", "b")]
    for x, (a, b, ink) in zip(xs, rows):
        out.append(label(x, cy + R + 34, a, f"lab {ink}", "middle"))
        out.append(label(x, cy + R + 54, b, f"num {ink}" if ink != "w" else "num m", "middle"))
    return "".join(out)


MOTIFS = {
    "bytes": m_bytes, "fields": m_fields, "rows": m_rows,
    "ask": m_ask, "batch": m_batch, "abstain": m_abstain,
}
