#!/usr/bin/env python3
"""gen.py - generate the Corpus Hub static site (hub.xerj.org) from the registry.

Reads ONLY registry state on this branch (corpus-hub):
  tools/xerj-code/hub/*.json                 - manifests (lane A)
  tools/xerj-code/hub/backlog/backlog-100.json - categories/status/use-cases/tags
  tools/xerj-code/hub/WAVES.md               - G7 scores per wave
  tools/xerj-code/hub/backlog/*-graded.json  - graded G7 suites (non-IT lane)
  tools/xerj-code/hub/backlog/impact-snapshot-*.json - benchmark A/B snapshot
  tools/packs/*/recipe.toml                  - lane B packs
Writes out/ (index, per-corpus, per-category, about, style, data.json).

Design: xerj.org brandbook, day palette only (user directive 2026-10-06:
strictly main-site fonts and CSS, domains as large typography, white default).
Copy: plain short sentences, no em-dashes, no filler intros (user directive
2026-10-06). Stdlib only; deterministic; no network. Rerun + redeploy on
registry change.
"""
import datetime as dt
import hashlib
import html
import json
import pathlib
import re
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[2]
HUB = ROOT / "tools" / "xerj-code" / "hub"
PACKS = ROOT / "tools" / "packs"
OUT = pathlib.Path(__file__).resolve().parent / "out"

CATS = {
    "SEC": ("Security and vulnerabilities", "Is this CVE exploited; does this pattern have KEV/GHSA precedent; how was it fixed"),
    "DB": ("Databases and storage engines", "How duckdb, rocksdb, sqlite, kafka, faiss actually do planner, LSM, B-tree, log, ANN"),
    "NET": ("Networking, protocols and serialisation", "How curl, nginx, envoy, protobuf, zstd do HTTP, proxying, framing, compression"),
    "CRYPTO": ("Cryptography, TLS and secrets", "How openssl, rustls, s2n-tls, wireguard, vault do handshakes, ciphers, key handling"),
    "WEB": ("Web and frontend platform", "What CSS, the DOM, ECMAScript, a design system or CLDR actually specify"),
    "API": ("API design and specification", "What a reviewed REST/GraphQL/OpenAPI surface looks like, and the rule behind it"),
    "OPS": ("Operations and SRE", "How real teams run, observe and recover systems; what the runbook says"),
    "LAW": ("Law and regulation", "What the statute, CFR title, EU regulation or building/traffic code actually says"),
    "HEALTH": ("Health and clinical reference", "What the clinical code, drug label or food-safety rule actually requires"),
    "AI": ("AI, prompts and agents", "A tested prompt, a prompt-engineering pattern, or an agent design to build on"),
    "WRITE": ("Writing and copy", "A proven content-style rule, copy pattern or editorial standard to write to"),
}

STATUSES = {"live": ("live", "ok"), "candidate": ("candidate", "warn"), "planned": ("planned", "mute"),
            "deferred": ("deferred", "warn"), "killed": ("killed", "bad"), "withdrawn": ("withdrawn", "bad")}

STAMP = "2026-10-13"


def plain(s):
    """No em/en dashes in generated copy (user directive 2026-10-06).
    Registry text keeps its bytes; rendering normalizes."""
    s = str(s)
    s = s.replace(" — ", ": ").replace(" —", ":").replace("— ", ":")
    s = s.replace("—", "-")
    s = s.replace(" – ", ": ").replace("–", "-")
    return s


def esc(s):
    return html.escape(plain(s), quote=True)


def human_bytes(n):
    if not n:
        return "0 B"
    for unit in ("B", "KB", "MB", "GB"):
        if n < 1024 or unit == "GB":
            return f"{n:.0f} {unit}" if unit == "B" else f"{n:.1f} {unit}"
        n /= 1024
    return f"{n:.1f} GB"


def human_files(n):
    return f"{n:,} files" if n else "0 files"


def parse_waves_g7():
    """WAVES.md G7 tables -> {corpus: {median, verdict, wave}}"""
    scores = {}
    text = (HUB / "WAVES.md").read_text()
    for m in re.finditer(r"^## (Wave [^-\n]+).*?\n(.*?)(?=^## |\Z)", text, re.M | re.S):
        wave = m.group(1).strip()
        for line in m.group(2).splitlines():
            c = [x.strip() for x in line.strip().strip("|").split("|")]
            if len(c) >= 3 and c[0] and not c[0].startswith(("-", "corpus")):
                try:
                    median = float(c[1].split()[0])
                except (ValueError, IndexError):
                    continue
                verdict = "pass" if "pass" in c[2] else c[2] or "n/a"
                scores.setdefault(c[0], {"median": median, "verdict": verdict, "wave": wave})
    return scores


def suite_median(queries):
    """Relevant count of 5, computed when a suite carries no median.
    pass/relevant = 1, partial = 0.5; suite-defect queries are excluded
    from numerator and denominator both."""
    num = {"pass": 1.0, "relevant": 1.0, "partial": 0.5}
    got = [num.get(q.get("verdict") or q.get("grade"), 0.0)
           for q in queries
           if "excluded" not in str(q.get("verdict") or q.get("grade"))]
    if not got:
        return 0.0
    return sum(got) / len(got) * 5.0


def parse_graded_suites():
    """backlog/*graded*.json -> {corpus: {queries:[...], median}}"""
    out = {}
    for p in sorted((HUB / "backlog").glob("*graded*.json")):
        try:
            data = json.loads(p.read_text())
        except json.JSONDecodeError:
            continue
        # A graded file may carry a non-dict "graded" value (e.g. a timestamp
        # string describing when it was graded); it then is not a slug->suite
        # map. Skip it rather than crashing the whole site build on .items() -
        # one odd file must not freeze every corpus page in the deploy.
        container = data.get("graded", data.get("suites", data.get("corpora", {})))
        if not isinstance(container, dict):
            continue
        for slug, suite in container.items():
            if isinstance(suite, dict) and "queries" in suite:
                if "median" not in suite or suite["median"] is None:
                    suite["median"] = suite_median(suite["queries"])
                out[slug] = suite
    return out


def load_impact():
    snaps = sorted((HUB / "backlog").glob("impact-snapshot-*.json"))
    if not snaps:
        return {}, None
    data = json.loads(snaps[-1].read_text())
    return {r["corpus"]: r for r in data.get("rows", [])}, data.get("provenance", {})


def load_registry():
    backlog = json.loads((HUB / "backlog" / "backlog-100.json").read_text())
    rows = {r["slug"]: r for r in backlog["corpora"]}
    corpora = []
    # lane A manifests
    for p in sorted(HUB.glob("*.json")):
        if p.stem in ("TEMPLATE", "corpus-schema"):
            continue
        m = json.loads(p.read_text())
        slug = m["corpus"]
        row = rows.get(slug, {})
        corpora.append({
            "slug": slug, "lane": "A", "cat": row.get("cat", "?"),
            "status": row.get("status", "unknown"), "refresh": row.get("refresh", "pinned"),
            "usecase": row.get("domain", ""), "repos": m.get("repos", []),
            "added": m.get("cloned_at", ""), "kind": "reference corpus",
            "consumed": rows.get(slug, {}).get("sources", [{}])[0].get("url", ""),
            "tags": row.get("tags", {}), "merged_into": row.get("merged_into", ""),
        })
    # lane B recipes
    for p in sorted(PACKS.glob("*/recipe.toml")):
        if p.parent.name == "TEMPLATE-recipe":
            continue
        r = tomllib.loads(p.read_text())
        meta = r.get("recipe", {})
        slug = p.parent.name
        row = rows.get(slug, {})
        corpora.append({
            "slug": slug, "lane": "B", "cat": row.get("cat", "?"),
            "status": row.get("status", "unknown"), "refresh": row.get("refresh", "weekly"),
            "usecase": row.get("domain", ""), "repos": [],
            "sources": r.get("sources", []), "added": "", "kind": "curated pack",
            "consumed": meta.get("notes", "") or row.get("sources", [{}])[0].get("url", ""),
            "tags": row.get("tags", {}), "merged_into": row.get("merged_into", ""),
        })
    # backlog-only rows (planned/deferred/candidates with no manifest yet)
    have = {c["slug"] for c in corpora}
    for slug, row in rows.items():
        if slug not in have:
            corpora.append({
                "slug": slug, "lane": row.get("lane", "?"), "cat": row.get("cat", "?"),
                "status": row.get("status", "?"), "refresh": row.get("refresh", ""),
                "usecase": row.get("domain", ""), "repos": [], "sources": row.get("sources", []),
                "added": "", "kind": "backlog row", "consumed": row.get("sources", [{}])[0].get("url", ""),
                "tags": row.get("tags", {}), "merged_into": row.get("merged_into", ""),
            })
    # A slug can appear as both a lane-A manifest and a lane-B recipe (same
    # corpus offered two ways); keep ONE entry so it is never listed twice.
    # Richer wins: most pinned bytes, then most repos, then lane A.
    def rank(c):
        return (sum(r.get("bytes") or 0 for r in c["repos"]), len(c["repos"]),
                1 if c["lane"] == "A" else 0)
    best = {}
    for c in corpora:
        if c["slug"] not in best or rank(c) > rank(best[c["slug"]]):
            best[c["slug"]] = c
    return sorted(best.values(), key=lambda c: c["slug"])


def corpus_stats(c):
    files = sum(r.get("files") or 0 for r in c["repos"])
    nbytes = sum(r.get("bytes") or 0 for r in c["repos"])
    licences = sorted({r["review"]["spdx"] for r in c["repos"] if r.get("review")})
    if not licences:
        licences = sorted({s.get("licence", "?") for s in c.get("sources", [])[:1]})
    return files, nbytes, licences


def region(c):
    return (c.get("tags") or {}).get("region", "")


def tag_line(c):
    """Card tag line: REGION · TOPIC · TOPIC (subtopics in the tooltip)."""
    t = c.get("tags") or {}
    parts = []
    if t.get("region"):
        parts.append(t["region"].upper())
    for x in t.get("topics", [])[:2]:
        parts.append(x.upper())
    sub = t.get("subtopics", [])
    tip = ", ".join(sub) if sub else ""
    if not parts:
        return ""
    return f'<span class="ctags" title="{esc(tip)}">{esc(" · ".join(parts))}</span>'


def g7_badge(slug, g7, graded):
    if slug in graded:
        med = graded[slug].get("median") or 0.0
        cls = "ok" if med >= 3 else "bad"
        return f'<span class="badge g7 {cls}" title="retrieval spot-check, relevant of 5 queries">G7 {med:.1f}/5</span>'
    if slug in g7:
        g = g7[slug]
        cls = "ok" if g["verdict"] == "pass" else "bad"
        tip = esc(g["wave"])
        return f'<span class="badge g7 {cls}" title="retrieval spot-check ({tip})">G7 {g["median"]:.1f}/5</span>'
    return '<span class="badge g7 mute" title="not yet spot-checked">G7 n/a</span>'


def impact_badge(c, impact):
    r = impact.get(c["slug"])
    if not r or r.get("status") != "measured":
        return ""
    return (f'<span class="badge imp" title="drift-anchored agent A/B: bare pass then corpus pass">'
            f'A/B {esc(r["p"])} to {esc(r["x"])}</span>')


def card(c, g7, graded, impact):
    files, nbytes, licences = corpus_stats(c)
    st, stcls = STATUSES.get(c["status"], (c["status"], "mute"))
    return f'''<a class="card" data-cat="{esc(c['cat'])}" data-region="{esc(region(c))}" href="corpus/{esc(c['slug'])}.html">
  <div class="chead"><span class="cname">{esc(c['slug'])}</span><span class="dot {stcls}"></span></div>
  <p class="cuse">{esc(plain(c['usecase'])[:180])}{'...' if len(c['usecase']) > 180 else ''}</p>
  {tag_line(c)}
  <div class="cmeta">{'<span>merged into ' + esc(c['merged_into']) + '</span>' if c.get('merged_into') else '<span>' + human_bytes(nbytes) + '</span><span>' + human_files(files) + '</span>'}<span>{st}</span></div>
  <div class="cbadges"><span class="badge cat">{esc(c['cat'])}</span><span class="badge lane">{esc(c['lane'])}</span>{g7_badge(c['slug'], g7, graded)}{impact_badge(c, impact)}</div>
</a>'''


# Corpus Hub v2 (docs/CORPUS_HUB_V2_IMPLEMENTATION.md section 9). An agent asks ONE
# line and XERJ fetches the signed, pre-indexed pack for it, so nothing is cloned
# or indexed on the agent's machine: no manifest is downloaded and no corpus is
# "added". Every command printed on this site is therefore the one line, its
# `--def` form, or `xerj hub pull` for a large pack. Do not bring
# `xerj corpus add --from` back into a prompt or a "Use it" block.
#
# A copy prompt and a "Use it" block are offered only for a live, lane-A corpus:
# that is the set the pack builder reads a manifest for. A lane-B curated pack
# has no hub pack, so there is nothing to ask.
#
# The one line needs a binary that has `xerj ask`. Every prompt opens with
# `xerj ask --help`, so an agent on an older binary finds out at step 1.

# The catalogue, one line per corpus: linked from every page and every prompt.
CATALOGUE_URL = "https://hub.xerj.org/llms.txt"

# Step 1, shared by every prompt. Each step names what success looks like, so an
# agent can tell a working step from a silent failure.
CLI_STEP = (
    "1. Check the CLI: xerj ask --help\n"
    '   Success: the first line is usage: xerj ask "<question>" [options]\n'
    "   If xerj is missing: curl -fsSL https://xerj.org/get | sh\n"
    '   then: export PATH="$HOME/.local/bin:$PATH"\n'
    "   If xerj runs but does not know ask, it is an older build: reinstall with the same curl line.\n"
)

# The MCP route, shared by every prompt. A hub tool reads only the catalogue and
# packs already on the machine (xerj-mcp/src/hub.rs): it never downloads.
MCP_NOTE = (
    "The MCP tools read only packs already on this machine and never download one; "
    "when the pack is missing they return the xerj hub pull <slug> line to run in a shell once.\n"
)

# The node rule, shared by every prompt. XERJ finds or starts the node itself.
NODE_NOTE = (
    "XERJ uses the node at --url, $XERJ_URL or localhost:9200 if one answers; otherwise it starts its own. "
    "Stop only a node you started (xerj hub stop). Never pkill or killall xerj - "
    "other XERJ nodes may be running on this machine. "
    "If a pull fails with not in limits.snapshot_repo_allowlist, set XERJ_URL=http://127.0.0.1:9399 "
    "(a loopback port nothing listens on) and ask again: XERJ starts its own node, which can restore packs.\n"
)


def answer_lines(corpus):
    """What a good answer looks like, in the exact shape xerj_common::hub::render
    prints: per hit a header line (corpus, pin date, age, licence, file:line) and
    the passage, then where the answer came from. The CLI has no hosted client,
    so that last line is always `local pack`."""
    return (
        "   Success: per hit, a header line and the passage, then a last line:\n"
        f"   {corpus}  pin <date> (<N> days old)  <licence>  <file>:<line>\n"
        "   <the passage, or the declaration>\n"
        "   answered from: local pack\n"
    )


def install_prompt(c):
    """A paste-ready prompt for ONE corpus: the one line pinned to it, its
    `--def` form, and what success looks like. No manifest, no clone, no index."""
    slug = c["slug"]
    use = plain(c.get("usecase") or "")[:200]
    return (
        f'Use the "{slug}" reference corpus from the XERJ Corpus Hub '
        f"(https://hub.xerj.org/corpus/{slug}) for this task."
        + (f" What it is for: {use}" if use else "") + "\n"
        "Run these in a shell, in order, and check each result before the next:\n"
        + CLI_STEP +
        f'2. Ask: xerj "<your question>" --corpus {slug}\n'
        f'   A question of one word needs ask: xerj ask "<word>" --corpus {slug}\n'
        + answer_lines(slug) +
        f"3. For an exact value, signature or default: xerj ask --def <SYMBOL> --corpus {slug}\n"
        "   Success: the declaration, then a value: line for a constant or macro. No passages.\n"
        "Answer from what it printed: name the corpus, the pin date and the licence, and cite file:line. "
        "If it prints no match, say the corpus does not cover it rather than answering from memory.\n"
        + NODE_NOTE +
        "No shell? Call the MCP tool xerj_hub_ask with "
        f'{{"question": "<your question>", "corpus": "{slug}"}}.\n'
        + MCP_NOTE +
        f"Catalogue of every corpus, one line each: {CATALOGUE_URL}"
    )


def category_prompt(k, title, live):
    """One paste-ready prompt PER DOMAIN. It lists the domain's corpora with
    their sizes for context; the agent does not have to pick one, because XERJ
    routes the question to the right corpus and names it in the answer."""
    listing = "\n".join(
        f"- {c['slug']} ({human_bytes(corpus_stats(c)[1])}): {plain(c.get('usecase') or '')[:110]}"
        for c in live
    )
    return (
        f"The XERJ Corpus Hub {k} domain ({title}) has {len(live)} reference corpora. "
        "You do not have to name one: XERJ routes the question to the right corpus and says which. "
        "Add --corpus <slug> only to pin one.\n"
        f"{listing}\n"
        "Run these in a shell, in order, and check each result before the next:\n"
        + CLI_STEP +
        '2. Ask: xerj "<your question>"\n'
        '   A question of one word needs ask: xerj ask "<word>"\n'
        + answer_lines("<corpus>") +
        "   Exit 4 means more than one corpus fits: the candidates print, so run it again with --corpus <slug>.\n"
        "   No match followed by a closest corpora: line means routing was not sure: ask again with --corpus <slug>.\n"
        "3. For an exact value, signature or default: xerj ask --def <SYMBOL>\n"
        "   Success: the declaration, then a value: line for a constant or macro. No passages.\n"
        "Answer from what it printed: name the corpus, the pin date and the licence, and cite file:line. "
        "If it prints no match, say the hub does not cover it rather than answering from memory.\n"
        + NODE_NOTE +
        'No shell? Call the MCP tool xerj_hub_ask with {"question": "<your question>"}.\n'
        + MCP_NOTE +
        f"Catalogue of every corpus, one line each: {CATALOGUE_URL}"
    )


def tile(c):
    """One treemap cell: a corpus. Carries data-bytes; the squarify script
    (treemap.js) turns each domain band into a gap-free, space-filling treemap
    where every cell's AREA is proportional to the corpus's byte total. No
    inline size here: positions are computed client-side and recomputed on
    resize and on filter, so the layout is fully scalable."""
    files, nbytes, _ = corpus_stats(c)
    st, stcls = STATUSES.get(c["status"], (c["status"], "mute"))
    if c["status"] == "live":
        meta = f"{human_bytes(nbytes)} · {human_files(files)}" if nbytes else "curated pack"
    else:
        meta = st
    ttl = f'{c["slug"]} — {meta}'
    return (f'<div class="tile" data-cat="{esc(c["cat"])}" '
            f'data-region="{esc(region(c))}" data-name="{esc(c["slug"])}" '
            f'data-bytes="{nbytes}" title="{esc(ttl)}">'
            f'<span class="tdot {stcls}" title="{esc(st)}"></span>'
            f'<a class="tlink" href="corpus/{esc(c["slug"])}.html">'
            f'<span class="tn">{esc(c["slug"])}</span>'
            f'<span class="ts">{esc(meta)}</span></a></div>')


# Brandbook tokens from xerj.org (landing/style.css), day palette only.
# White schema is the one scheme: no dark variant, no prefers-color-scheme
# override (user directive 2026-10-06).
STYLE = """/* corpus hub - xerj.org brandbook, day palette, white only */
:root{
--z-bg:#f6f4ee;--z-ink:#11120f;--z-mute:#696762;--z-faint:#e3dfd4;--z-line:#cfcbbf;
--z-accent:#7f5200;--z-cmp:#6f8aa8;
--ok:#2e6b3a;--bad:#8c2f2f;--warn:#8a6d1a;
--font-display:'Big Shoulders Display','Inter',system-ui,sans-serif;
--font-prose:'Inter',system-ui,sans-serif;
--font-data:'IBM Plex Sans','Inter',system-ui,sans-serif;
--font-mono:'JetBrains Mono','IBM Plex Mono',monospace;
--fs-11:11px;--fs-13:13px;--fs-16:16px;--fs-20:20px;--fs-32:32px;
--fs-56:56px;--fs-96:96px;--fs-160:160px;
--track-ui:.14em;--track-label:.08em;
--sp-1:4px;--sp-2:8px;--sp-3:12px;--sp-4:16px;--sp-5:20px;--sp-6:24px;
--sp-8:32px;--sp-10:48px;--sp-12:64px;--sp-16:96px;
--page-x:64px;--max-w:1680px;
}
*{box-sizing:border-box}
body{margin:0;font-family:var(--font-prose);background:var(--z-bg);color:var(--z-ink);
line-height:1.5;font-size:var(--fs-16)}
a{color:inherit}
code{font-family:var(--font-mono);font-size:.92em;background:var(--z-faint);padding:.1em .35em}
/* nav: main-site pattern, 1px bottom line */
.nav{display:flex;align-items:baseline;gap:var(--sp-5);padding:var(--sp-4) var(--page-x);
border-bottom:1px solid var(--z-line);font-family:var(--font-data);font-size:var(--fs-11);
font-weight:600;text-transform:uppercase;letter-spacing:var(--track-ui);flex-wrap:wrap;
position:sticky;top:0;z-index:50;background:var(--z-bg)}
.nav .brand{font-family:var(--font-display);font-weight:900;font-size:var(--fs-16);
letter-spacing:.24em;color:var(--z-ink);text-decoration:none}
.nav .brand b{color:var(--z-accent)}
.nav a{color:var(--z-mute);text-decoration:none}
.nav a:hover{color:var(--z-ink)}
.nav .spacer{flex:1}
.nav a.navicon{display:inline-flex;align-items:center;color:var(--z-mute)}
.nav a.navicon svg{width:15px;height:15px;fill:currentColor;display:block}
.nav a.navicon:hover{color:var(--z-ink)}
/* contribute CTA: subtle 1px frame while the hero CTA is on screen; turns into the
   animated purple gradient (.on, set by JS) once the hero CTA scrolls up under the nav */
.nav a.navcta{color:var(--z-mute);border:1px solid var(--z-line);padding:.42em .95em;
background:none;transition:color .2s,border-color .2s}
.nav a.navcta:hover{color:var(--z-ink);border-color:var(--z-mute)}
.nav a.navcta.on{color:#fff;border:0;
background:linear-gradient(100deg,#6d28d9,#9333ea,#7c3aed,#6d28d9);background-size:280% 100%;animation:ctaflow 5s ease infinite}
.nav a.navcta.on:hover{color:#fff;filter:brightness(1.12)}
.cta{display:inline-block;margin-top:var(--sp-5);font-family:var(--font-data);font-size:var(--fs-13);
font-weight:600;text-transform:uppercase;letter-spacing:var(--track-ui);text-decoration:none;color:#fff;
background:linear-gradient(100deg,#6d28d9,#9333ea,#7c3aed,#6d28d9);background-size:280% 100%;animation:ctaflow 5s ease infinite;padding:.72em 1.25em;border:0}
.cta:hover{filter:brightness(1.12)}
@keyframes ctaflow{0%{background-position:0% 50%}50%{background-position:100% 50%}100%{background-position:0% 50%}}
.promptbox{border:1px solid var(--z-line);background:#fff;margin:var(--sp-4) 0}
.pbh{display:flex;justify-content:flex-end;padding:var(--sp-3) var(--sp-3) 0}
.prompt{font-family:var(--font-mono);font-size:var(--fs-13);line-height:1.65;color:var(--z-ink);
white-space:pre-wrap;word-break:break-word;padding:var(--sp-4) var(--sp-5) var(--sp-5);margin:0}
main{max-width:var(--max-w);margin:0 auto;padding:var(--sp-8) var(--page-x) var(--sp-16)}
/* type */
.kicker{font-family:var(--font-data);font-size:var(--fs-11);font-weight:500;
letter-spacing:var(--track-ui);text-transform:uppercase;color:var(--z-mute)}
.kicker .accent{color:var(--z-accent)}
.kicker .dash{opacity:.4;margin:0 6px}
h1.hero{font-family:var(--font-display);font-weight:900;font-size:clamp(40px,6vw,var(--fs-96));
line-height:.9;letter-spacing:-.02em;margin:0 0 var(--sp-6);color:var(--z-ink)}
h1.hero .accent{color:var(--z-accent)}
h2.scene{font-family:var(--font-display);font-weight:900;font-size:var(--fs-56);
line-height:.95;letter-spacing:-.005em;margin:0 0 var(--sp-6)}
p.lead{font-family:var(--font-prose);color:var(--z-mute);font-size:var(--fs-20);
line-height:1.5;max-width:56ch}
.note{color:var(--z-mute);font-size:var(--fs-13)}
/* hero row: hero text left, stats as an infographic quadrant on the right */
.herorow{display:flex;flex-wrap:wrap;gap:var(--sp-6) var(--sp-8);
align-items:flex-start;justify-content:space-between;margin-bottom:var(--sp-6)}
.heromain{flex:1 1 440px;min-width:0}
.heromain .lead{margin-bottom:0;max-width:52ch}
.schema{margin:var(--sp-6) 0 var(--sp-8)}
.arch{list-style:none;margin:0;padding:0;display:flex;gap:var(--sp-6);align-items:stretch}
.arch .node{position:relative;flex:1 1 0;min-width:0;border:1px solid var(--z-line);
background:var(--z-bg);padding:var(--sp-4) var(--sp-5);display:flex;flex-direction:column;gap:6px}
.arch .node.on{border-color:var(--z-accent);box-shadow:inset 0 0 0 1px var(--z-accent)}
.arch .node:not(:last-child)::after{position:absolute;right:calc(var(--sp-6) / -2 - 5px);
top:50%;transform:translateY(-50%);font-family:var(--font-display);font-weight:900;
color:var(--z-accent);font-size:var(--fs-20);line-height:1}
.arch .node:nth-child(1)::after{content:"\\2192"}
.arch .node:nth-child(2)::after{content:"\\2190"}
.arch .nlabel{font-family:var(--font-display);font-weight:800;font-size:var(--fs-20);
color:var(--z-ink);line-height:1}
.arch .nsub{font-family:var(--font-prose);color:var(--z-mute);font-size:var(--fs-13);line-height:1.4}
.queries{margin-top:var(--sp-4);display:flex;flex-wrap:wrap;align-items:center;gap:8px}
.queries .qlab{font-family:var(--font-data);font-size:var(--fs-11);font-weight:600;
text-transform:uppercase;letter-spacing:var(--track-ui);color:var(--z-mute)}
.queries code{font-family:var(--font-mono);font-size:var(--fs-13);color:var(--z-ink);
background:#fff;border:1px solid var(--z-line);padding:3px 8px}
@media(max-width:720px){.arch{flex-direction:column;gap:var(--sp-6)}
.arch .node:not(:last-child)::after{right:auto;left:50%;top:auto;
bottom:calc(var(--sp-6) / -2 - 5px);transform:translateX(-50%)}
.arch .node:nth-child(1)::after{content:"\\2193"}
.arch .node:nth-child(2)::after{content:"\\2191"}}
.heroside{flex:0 1 360px;display:flex;flex-direction:column;gap:var(--sp-4);margin-top:var(--sp-2)}
.statgrid{display:grid;grid-template-columns:1fr 1fr;grid-auto-rows:1fr;
gap:1px;background:var(--z-line);border:1px solid var(--z-line)}
.heroside .cta{margin-top:0;text-align:center}
.stat{background:var(--z-bg);padding:var(--sp-4) var(--sp-5);min-height:88px;
display:flex;flex-direction:column;justify-content:center;gap:4px}
.stat.wide{grid-column:1 / -1}
.stat b{font-family:var(--font-display);font-weight:800;white-space:nowrap;
font-size:var(--fs-32);line-height:1;color:var(--z-ink)}
.stat span{font-family:var(--font-data);font-size:var(--fs-11);font-weight:500;
text-transform:uppercase;letter-spacing:var(--track-ui);color:var(--z-mute)}
.count{font-family:var(--font-data);font-size:var(--fs-11);font-weight:500;
text-transform:uppercase;letter-spacing:var(--track-ui);color:var(--z-mute);margin-left:auto}
/* domains: the front page centrepiece, giant display words */
.domains{margin:var(--sp-8) 0}
.domain{display:grid;grid-template-columns:110px 1fr auto 40px;align-items:center;gap:var(--sp-6);
padding:var(--sp-5) 0;border-top:1px solid var(--z-line);text-decoration:none}
.domain:last-child{border-bottom:1px solid var(--z-line)}
.domain .dcount{font-family:var(--font-mono);font-size:var(--fs-20);color:var(--z-mute)}
.domain .dcount b{color:var(--z-ink);font-weight:700}
.domain .dword{font-family:var(--font-display);font-weight:900;
font-size:clamp(40px,6vw,var(--fs-96));line-height:.92;letter-spacing:-.01em;
color:var(--z-ink);text-transform:uppercase}
.domain .dinfo{text-align:right;max-width:46ch}
.domain .dinfo b{display:block;font-family:var(--font-data);font-size:var(--fs-13);
font-weight:600;text-transform:uppercase;letter-spacing:var(--track-label);color:var(--z-ink)}
.domain .dinfo span{font-family:var(--font-prose);font-size:var(--fs-13);color:var(--z-mute)}
.domain .dgo{font-family:var(--font-mono);font-size:var(--fs-20);color:var(--z-mute)}
.domain:hover .dword{color:var(--z-accent)}
.domain:hover .dgo{color:var(--z-accent)}
/* toolbar: square 1px inputs and chips, no radius */
.toolbar{display:flex;flex-direction:column;gap:var(--sp-3);margin:var(--sp-4) 0}
.toolbar .row{display:flex;gap:var(--sp-2);flex-wrap:wrap;align-items:center}
.toolbar input{font-family:var(--font-prose);font-size:var(--fs-13);padding:.5em .8em;
border:1px solid var(--z-line);background:transparent;color:var(--z-ink);min-width:22rem}
.toolbar input:focus{outline:none;border-color:var(--z-accent)}
.chip{font-family:var(--font-data);font-size:var(--fs-11);font-weight:500;
text-transform:uppercase;letter-spacing:var(--track-label);border:1px solid var(--z-line);
background:transparent;color:var(--z-mute);padding:.35em .9em;cursor:pointer}
.chip:hover{border-color:var(--z-ink);color:var(--z-ink)}
.chip.on{background:var(--z-ink);color:var(--z-bg);border-color:var(--z-ink)}
/* cards: 1px square, no radius, no shadow */
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(330px,1fr));gap:1px;
background:var(--z-line);border:1px solid var(--z-line);margin-top:var(--sp-4)}
.card{display:flex;flex-direction:column;gap:var(--sp-2);text-decoration:none;
background:var(--z-bg);padding:var(--sp-4)}
.card:hover{background:var(--z-faint)}
.chead{display:flex;justify-content:space-between;align-items:center}
.cname{font-family:var(--font-mono);font-weight:600;font-size:var(--fs-13)}
.dot{width:.65em;height:.65em;border-radius:50%;display:inline-block}
.dot.ok{background:var(--ok)}.dot.warn{background:var(--warn)}
.dot.bad{background:var(--bad)}.dot.mute{background:var(--z-mute)}
.cuse{color:var(--z-mute);font-size:var(--fs-13);margin:0;min-height:2.8em}
.ctags{font-family:var(--font-mono);font-size:var(--fs-11);letter-spacing:var(--track-label);
color:var(--z-accent)}
.cmeta{display:flex;gap:var(--sp-4);color:var(--z-mute);font-size:var(--fs-11);
font-family:var(--font-mono)}
.cbadges{display:flex;gap:var(--sp-1);flex-wrap:wrap}
.badge{font-family:var(--font-mono);font-size:var(--fs-11);border:1px solid var(--z-line);
padding:.1em .5em;color:var(--z-mute)}
.badge.cat{color:var(--z-accent);border-color:var(--z-accent)}
.badge.g7.ok,.badge.imp{color:var(--ok);border-color:var(--ok)}
.badge.g7.bad{color:var(--bad);border-color:var(--bad)}
.badge.g7.mute{opacity:.6}
/* detail pages */
section.detail h2{font-family:var(--font-display);font-weight:700;font-size:var(--fs-32);
line-height:.95;border-bottom:1px solid var(--z-line);padding-bottom:var(--sp-2);margin:var(--sp-10) 0 var(--sp-4)}
table{border-collapse:collapse;width:100%;font-size:var(--fs-13);font-family:var(--font-prose)}
th,td{text-align:left;padding:.45em .6em;border-bottom:1px solid var(--z-line);vertical-align:top}
th{color:var(--z-mute);font-weight:600;font-family:var(--font-data);font-size:var(--fs-11);
text-transform:uppercase;letter-spacing:var(--track-label)}
td.mono{font-family:var(--font-mono);font-size:var(--fs-13)}
.kbox{border:1px solid var(--z-line);padding:var(--sp-4) var(--sp-5);margin:var(--sp-4) 0}
.g7q{margin:var(--sp-3) 0;padding:var(--sp-3) var(--sp-4);border-left:3px solid var(--z-line)}
.g7q.pass{border-color:var(--ok)}.g7q.fail{border-color:var(--bad)}
.backlink{font-family:var(--font-mono);font-size:var(--fs-13);color:var(--z-mute);text-decoration:none}
.backlink:hover{color:var(--z-ink)}
h1.slug{font-family:var(--font-mono);font-size:var(--fs-32);font-weight:700;margin:var(--sp-4) 0 0}
/* footer: main-site pattern */
footer{border-top:1px solid var(--z-line);color:var(--z-mute);padding:var(--sp-5) var(--page-x);
font-family:var(--font-data);font-size:var(--fs-11);font-weight:500;text-transform:uppercase;
letter-spacing:var(--track-ui);display:flex;gap:var(--sp-8);flex-wrap:wrap}
footer a{color:var(--z-mute);text-decoration:none}
footer a:hover{color:var(--z-ink)}
/* treemap: domains as tiled groups, corpora as size-weighted tiles.
   1px lines, square, day palette — same brandbook as the cards. */
.treemap{display:flex;flex-direction:column;gap:1px;background:var(--z-line);
border:1px solid var(--z-line);margin-top:var(--sp-4)}
.tgroup{display:flex;flex-direction:column;background:var(--z-bg)}
.thead{display:flex;align-items:center;justify-content:space-between;gap:var(--sp-3);
flex-wrap:wrap;padding:var(--sp-4) var(--sp-4) var(--sp-3);border-bottom:1px solid var(--z-line)}
.thword{font-family:var(--font-display);font-weight:900;
font-size:clamp(32px,4vw,var(--fs-56));line-height:.9;letter-spacing:-.01em;
color:var(--z-ink);text-transform:uppercase;text-decoration:none}
.thword:hover{color:var(--z-accent)}
.tgright{display:flex;align-items:center;gap:var(--sp-3);flex-wrap:wrap;justify-content:flex-end}
.tgmeta{font-family:var(--font-data);font-size:var(--fs-11);font-weight:500;
text-transform:uppercase;letter-spacing:var(--track-label);color:var(--z-mute);text-align:right}
.tgmeta b{color:var(--z-ink)}
/* space-filling treemap: band is a positioned box (height set per domain),
   cells are absolutely placed by treemap.js and fill it with no gaps. */
.tiles{position:relative;background:var(--z-line);width:100%}
.tile{position:absolute;display:flex;flex-direction:column;gap:2px;background:var(--z-bg);
padding:var(--sp-3);overflow:hidden;box-shadow:inset 0 0 0 1px var(--z-line)}
.tile:hover{background:var(--z-faint);z-index:2}
.tile .tdot{position:absolute;top:var(--sp-3);right:var(--sp-3);width:.6em;height:.6em;border-radius:50%}
.tile .tlink{display:flex;flex-direction:column;gap:2px;text-decoration:none;padding-right:var(--sp-4)}
.tile .tn{font-family:var(--font-mono);font-weight:600;font-size:var(--fs-13);
word-break:break-word;color:var(--z-ink)}
.tile:hover .tn{color:var(--z-accent)}
.tile .ts{font-family:var(--font-mono);font-size:var(--fs-11);color:var(--z-mute)}
.copy{font-family:var(--font-data);font-size:var(--fs-11);font-weight:600;
letter-spacing:var(--track-label);text-transform:uppercase;border:1px solid var(--z-line);
background:transparent;color:var(--z-mute);padding:.3em .7em;cursor:pointer}
.copy:hover{border-color:var(--z-accent);color:var(--z-accent)}
.copy.done{color:var(--ok);border-color:var(--ok)}
.tile .copy{align-self:flex-start;margin-top:auto}
@media (max-width:900px){
:root{--page-x:18px;--sp-12:40px;--sp-16:56px}
.toolbar input{min-width:100%}
.domain{grid-template-columns:1fr;gap:var(--sp-2)}
.domain .dinfo{text-align:left;max-width:none}
}
"""

FONTS = """<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Big+Shoulders+Display:wght@400;700;800;900&family=Inter:wght@400;500;600;700&family=IBM+Plex+Sans:wght@400;500;600;700&family=JetBrains+Mono:wght@400;500;700&display=swap" rel="stylesheet">"""

PAGE = """<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title><meta name="description" content="{desc}">
{fonts}
<link rel="stylesheet" href="{rel}assets/style.css?v={ver}"></head>
<body><nav class="nav"><a class="brand" href="https://xerj.org">XERJ<b>·</b>HUB</a>
<a href="{rel}index.html">CORPORA</a><a href="{rel}index.html#domains">DOMAINS</a>
<a href="{rel}about.html">ABOUT</a>
<a href="{rel}llms.txt" title="The catalogue: every live corpus, one line each">CATALOGUE</a>
<a href="https://github.com/xerj-org/xerj/tree/corpus-hub/tools/xerj-code/hub">REGISTRY (GIT)</a>
<span class="spacer"></span><a class="navicon" href="https://x.com/xerj_org" title="XERJ on X" target="_blank" rel="noopener"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M18.244 2.25h3.308l-7.227 8.26 8.502 11.24H16.17l-5.214-6.817L4.99 21.75H1.68l7.73-8.835L1.254 2.25H8.08l4.713 6.231zm-1.161 17.52h1.833L7.084 4.126H5.117z"/></svg></a><a class="navicon" href="https://discord.com/invite/yetXFusXfs" title="XERJ on Discord" target="_blank" rel="noopener"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M20.317 4.37a19.79 19.79 0 00-4.885-1.515.074.074 0 00-.079.037c-.21.375-.444.864-.608 1.25a18.27 18.27 0 00-5.487 0 12.64 12.64 0 00-.617-1.25.077.077 0 00-.079-.037A19.74 19.74 0 003.677 4.37a.07.07 0 00-.032.027C.533 9.046-.32 13.58.099 18.057a.082.082 0 00.031.057 19.9 19.9 0 005.993 3.03.078.078 0 00.084-.028c.462-.63.874-1.295 1.226-1.994a.076.076 0 00-.041-.106 13.1 13.1 0 01-1.872-.892.077.077 0 01-.008-.128 10.2 10.2 0 00.372-.292.074.074 0 01.077-.01c3.928 1.793 8.18 1.793 12.062 0a.074.074 0 01.078.01c.12.098.246.198.373.292a.077.077 0 01-.006.127 12.3 12.3 0 01-1.873.892.077.077 0 00-.041.107c.36.698.772 1.362 1.225 1.993a.076.076 0 00.084.028 19.84 19.84 0 006.002-3.03.077.077 0 00.032-.054c.5-5.177-.838-9.674-3.549-13.66a.061.061 0 00-.031-.03zM8.02 15.33c-1.183 0-2.157-1.085-2.157-2.419 0-1.333.956-2.419 2.157-2.419 1.21 0 2.176 1.096 2.157 2.42 0 1.333-.956 2.418-2.157 2.418zm7.975 0c-1.183 0-2.157-1.085-2.157-2.419 0-1.333.955-2.419 2.157-2.419 1.21 0 2.176 1.096 2.157 2.42 0 1.333-.946 2.418-2.157 2.418z"/></svg></a><a href="https://xerj.org">XERJ.ORG</a><a href="{rel}contribute.html" class="navcta">ADD YOUR CORPUS</a></nav>
{body}
<footer><span>XERJ · CORPUS HUB · {stamp}</span>
<span>PINNED SOURCES · VERIFIED LICENCES · MEASURED RETRIEVAL</span>
<span><a href="{rel}index.html">CORPORA</a> · <a href="{rel}about.html">ABOUT</a> ·
<a href="{rel}llms.txt">LLMS.TXT</a></span></footer>
{navjs}
</body></html>"""


# Keeps a single prominent CTA: the nav "ADD YOUR CORPUS" stays a subtle 1px frame
# while the hero CTA is on screen, and turns purple (.on) once the hero CTA scrolls
# under the sticky nav. On pages with no hero CTA it is prominent from the start.
NAVCTA_JS = """<script>
(function(){
  var nav=document.querySelector('.nav a.navcta'); if(!nav) return;
  var hero=document.querySelector('.heroside .cta');
  if(!hero){ nav.classList.add('on'); return; }
  var io=new IntersectionObserver(function(e){ nav.classList.toggle('on', !e[0].isIntersecting); },
    {rootMargin:'-64px 0px 0px 0px', threshold:0});
  io.observe(hero);
})();
</script>"""


# Content hash of the stylesheet: appended as ?v= on every page's CSS link so a
# changed stylesheet always busts the browser cache (assets carry a long max-age).
STYLE_VER = hashlib.sha1(STYLE.encode("utf-8")).hexdigest()[:8]


def page(title, desc, body, rel=""):
    return PAGE.format(title=esc(title), desc=esc(desc), body=body, rel=rel,
                       fonts=FONTS, stamp=STAMP, ver=STYLE_VER, navjs=NAVCTA_JS)


FILTER_JS = """<script>
// Squarified treemap (Bruls, Huizing, van Wijk): fills each domain band with
// zero gaps, every cell's area proportional to its corpus's bytes. Recomputed
// on load, on resize, and on filter, so the layout is fully scalable.
function _worst(row,side){
  var sum=0,mx=0,mn=Infinity,i;
  for(i=0;i<row.length;i++){var a=row[i].area;sum+=a;if(a>mx)mx=a;if(a<mn)mn=a;}
  var s2=sum*sum,d2=side*side;
  return Math.max(d2*mx/s2, s2/(d2*mn));
}
function _squarify(items,x,y,w,h,out){
  items=items.slice().sort(function(a,b){return b.area-a.area;});
  var rx=x,ry=y,rw=w,rh=h,idx=0;
  while(idx<items.length){
    var side=Math.min(rw,rh); if(side<=0)side=0.0001;
    var row=[];
    while(idx<items.length){
      var cand=row.concat([items[idx]]);
      if(row.length===0||_worst(cand,side)<=_worst(row,side)){row.push(items[idx]);idx++;}
      else break;
    }
    var rsum=0,i; for(i=0;i<row.length;i++)rsum+=row[i].area;
    var thick=rsum/side;
    if(rw>=rh){var cy=ry;
      for(i=0;i<row.length;i++){var hh=row[i].area/thick;out.push({el:row[i].el,x:rx,y:cy,w:thick,h:hh});cy+=hh;}
      rx+=thick;rw-=thick;
    }else{var cx=rx;
      for(i=0;i<row.length;i++){var ww=row[i].area/thick;out.push({el:row[i].el,x:cx,y:ry,w:ww,h:thick});cx+=ww;}
      ry+=thick;rh-=thick;
    }
  }
}
function _layoutBand(container){
  var W=container.clientWidth,H=container.clientHeight;
  if(!W||!H)return;
  var tiles=[].slice.call(container.querySelectorAll('.tile')).filter(function(t){return t.style.display!=='none';});
  if(!tiles.length)return;
  var raw=tiles.map(function(t){return Math.max(0,parseFloat(t.dataset.bytes)||0);});
  var mx=Math.max.apply(null,raw)||1, floor=mx*0.1;   // readability cap: no cell below 1/10 of the band's largest, so max:min area <= 10x
  var items=tiles.map(function(t,i){return {el:t,value:Math.max(raw[i],floor)};});
  var total=0;items.forEach(function(it){total+=it.value;});
  var scale=(W*H)/total,out=[];
  _squarify(items.map(function(it){return {el:it.el,area:it.value*scale};}),0,0,W,H,out);
  out.forEach(function(r){
    var s=r.el.style;
    s.left=(r.x/W*100)+'%';s.top=(r.y/H*100)+'%';
    s.width=(r.w/W*100)+'%';s.height=(r.h/H*100)+'%';
    var ts=r.el.querySelector('.ts');if(ts)ts.style.display=(r.h<48||r.w<84)?'none':'';
    var tn=r.el.querySelector('.tn');if(tn)tn.style.display=(r.h<26||r.w<46)?'none':'';
  });
}
function layoutAll(){[].slice.call(document.querySelectorAll('.tiles')).forEach(_layoutBand);}
var cards=[...document.querySelectorAll('.tile')];
var st={q:'',cat:'',region:''};
function apply(){
  cards.forEach(function(c){
    var ok=true;
    if(st.q&&c.textContent.toLowerCase().indexOf(st.q)<0)ok=false;
    if(st.cat&&c.dataset.cat!==st.cat)ok=false;
    if(st.region&&c.dataset.region!==st.region)ok=false;
    c.style.display=ok?'':'none';
  });
  document.querySelectorAll('.tgroup').forEach(function(g){
    var any=[].slice.call(g.querySelectorAll('.tile')).some(function(t){return t.style.display!=='none';});
    g.style.display=any?'':'none';
  });
  var n=0;cards.forEach(function(c){if(c.style.display!=='none')n++;});
  var el=document.getElementById('count');
  if(el)el.textContent=n+' of '+cards.length+' shown';
  layoutAll();
}
document.getElementById('q').addEventListener('input',function(e){st.q=e.target.value.toLowerCase();apply();});
document.querySelectorAll('.chip[data-cat]').forEach(function(b){
  b.addEventListener('click',function(){
    var on=b.classList.contains('on');
    document.querySelectorAll('.chip[data-cat]').forEach(function(x){x.classList.remove('on');});
    if(!on)b.classList.add('on');
    st.cat=on?'':b.dataset.cat;apply();
  });
});
document.querySelectorAll('.chip[data-region]').forEach(function(b){
  b.addEventListener('click',function(){
    var on=b.classList.contains('on');
    document.querySelectorAll('.chip[data-region]').forEach(function(x){x.classList.remove('on');});
    if(!on)b.classList.add('on');
    st.region=on?'':b.dataset.region;apply();
  });
});
layoutAll();
window.addEventListener('load',layoutAll);
var _rt;window.addEventListener('resize',function(){clearTimeout(_rt);_rt=setTimeout(layoutAll,120);});
</script>"""


COPY_JS = """<script>
document.querySelectorAll('.copy').forEach(function(b){
  b.addEventListener('click',function(e){
    e.preventDefault();e.stopPropagation();
    var t=b.getAttribute('data-copy')||'';
    function done(){var o=b.textContent;b.textContent='COPIED';b.classList.add('done');
      setTimeout(function(){b.textContent=o;b.classList.remove('done');},1400);}
    if(navigator.clipboard&&navigator.clipboard.writeText){
      navigator.clipboard.writeText(t).then(done,function(){});
    }else{
      var ta=document.createElement('textarea');ta.value=t;document.body.appendChild(ta);
      ta.select();try{document.execCommand('copy');done();}catch(_){}document.body.removeChild(ta);
    }
  });
});
</script>"""


def render_index(corpora, g7, graded, impact, prov):
    live = [c for c in corpora if c["status"] == "live"]
    total_bytes = sum(sum(r.get("bytes") or 0 for r in c["repos"]) for c in live)
    n_imp = sum(1 for c in live if impact.get(c["slug"], {}).get("status") == "measured")
    regions = sorted({region(c) for c in corpora if region(c)} - {""}, key=lambda r: (r == "GLOBAL", r))
    # Real, space-filling treemap: one band per domain (biggest domain first).
    # Each band is a squarified treemap (treemap.js) of its corpora that fills
    # the band with NO gaps, every cell's AREA proportional to the corpus's
    # bytes, but capped so the biggest cell is at most 10x the smallest (set in
    # treemap.js) — readability wins over exact proportion once the ratio is huge.
    # Each domain carries ONE copy-prompt that tells an agent how to ask any of
    # its live corpora with one line.
    groups = []
    dstats = {k: sum(corpus_stats(c)[1]
                     for c in corpora if c["cat"] == k and c["status"] == "live")
              for k in CATS}
    dmax = max(dstats.values(), default=0) or 1
    for k, (title, _what) in sorted(CATS.items(), key=lambda kv: -dstats[kv[0]]):
        # All LIVE corpora go in the treemap, sized by bytes. A lane-B curated
        # pack has no PINNED bytes (its recipe fetches at use time), so it shows
        # as a small, clearly-labelled "curated pack" cell (not "0 B"), kept
        # readable by the squarify floor. Candidates/deferred are not live and
        # never appear here.
        live_cs = sorted((c for c in corpora if c["cat"] == k and c["status"] == "live"),
                         key=lambda c: (-corpus_stats(c)[1], c["slug"]))
        if not live_cs:
            continue
        n_live = len(live_cs)
        cat_bytes = dstats[k]
        # Height gives every cell room to be readable: grows with the live
        # corpus count (crowded domains are taller) with a byte nudge so the
        # biggest domain still leads. Clamped 240..540px.
        band_h = max(240, min(540, round(120 + 13 * n_live + 110 * (cat_bytes / dmax) ** 0.5)))
        # The copy-prompt lists the pinned (lane-A) corpora; a lane-B curated
        # pack has no hub pack to ask, so it is left out of the prompt.
        installable = [c for c in live_cs if corpus_stats(c)[1] > 0]
        copy = (f'<button class="copy" data-copy="{esc(category_prompt(k, title, installable))}" '
                f'title="Copy a prompt that tells an agent how to ask the {esc(k)} corpora with one line">COPY PROMPT</button>')
        tiles = "".join(tile(c) for c in live_cs)
        groups.append(f'''<div class="tgroup">
<div class="thead"><a class="thword" href="category/{k}.html">{esc(k)}</a>
<div class="tgright">{copy}<span class="tgmeta"><b>{n_live}</b> live · {esc(title)} · {human_bytes(cat_bytes)}</span></div></div>
<div class="tiles" style="height:{band_h}px">{tiles}</div></div>''')
    treemap = "\n".join(groups)
    cat_chips = "".join(f'<button class="chip" data-cat="{k}">{k}</button>' for k in CATS)
    region_chips = "".join(f'<button class="chip" data-region="{esc(r)}">{esc(r)}</button>' for r in regions)
    body = f'''<main>
<div class="kicker"><span class="accent">HUB.XERJ.ORG</span><span class="dash">·</span><span>REFERENCE MEMORY HUB FOR AI AGENTS</span></div>
<div class="herorow">
<div class="heromain">
<h1 class="hero">REFERENCE MEMORY HUB<br><span class="accent">for AI agents</span></h1>
<p class="lead">Knowledge becomes memory. The hub pins a field&rsquo;s open knowledge into a memory
corpus an agent can search in seconds &mdash; so instead of guessing from training, any AI agent
that loads one works like a domain expert.</p>
</div>
<div class="heroside">
<div class="statgrid">
<div class="stat"><b>{len(live)}</b><span>live corpora</span></div>
<div class="stat"><b>{len(CATS)}</b><span>domains</span></div>
<div class="stat"><b>{len(corpora)}</b><span>registry rows</span></div>
<div class="stat"><b>{human_bytes(total_bytes)}</b><span>pinned text</span></div>
</div>
<a class="cta" href="contribute.html">Add your corpus &rarr;</a>
</div>
</div>
<section class="schema" aria-label="How domain knowledge becomes agent memory">
<ol class="arch">
<li class="node"><span class="nlabel">Domain knowledge</span><span class="nsub">everything a field has written down &mdash; docs, code, rules, research</span></li>
<li class="node on"><span class="nlabel">Memory corpus</span><span class="nsub">pinned and searchable, right here on the hub</span></li>
<li class="node"><span class="nlabel">AI agents</span><span class="nsub">now domain experts</span></li>
</ol>
</section>
<section id="domains">
<div class="toolbar">
<div class="row"><input id="q" type="search" placeholder="filter by name, topic, use-case, licence"><span id="count" class="count"></span></div>
<div class="row"><span class="kicker">DOMAIN</span><button class="chip on" data-cat=""></button>{cat_chips}</div>
<div class="row"><span class="kicker">REGION</span>{region_chips}</div>
</div>
<div class="treemap" id="grid">{treemap}</div>
</section>
</main>
{FILTER_JS}
{COPY_JS}'''
    return page("XERJ Corpus Hub: reference memory for AI agents",
                "Pinned, licence-checked, measured corpora an agent downloads per task and searches instead of guessing",
                body)


def detail_body(c, g7, graded, impact, prov):
    files, nbytes, licences = corpus_stats(c)
    st, stcls = STATUSES.get(c["status"], (c["status"], "mute"))
    cat_desc = CATS.get(c["cat"], ("", ""))
    t = c.get("tags") or {}
    tags_txt = " · ".join(([t["region"].upper()] if t.get("region") else [])
                          + [x.upper() for x in t.get("topics", [])]
                          + [x.upper() for x in t.get("subtopics", [])]) or "none"
    rows = []
    for r in c["repos"]:
        rv = r.get("review", {})
        rows.append(f'''<tr><td class="mono"><a href="{esc(r.get('url',''))}">{esc(r.get('repo',''))}</a></td>
<td class="mono">{esc(r.get('sha','')[:12])}</td><td>{esc(rv.get('spdx','?'))}</td>
<td class="mono">{human_bytes(r.get('bytes'))} / {human_files(r.get('files'))}</td>
<td class="note">{esc(plain(rv.get('note',''))[:400])}</td></tr>''')
    for s in c.get("sources", []):
        rows.append(f'''<tr><td class="mono"><a href="{esc(s.get('url',''))}">{esc(s.get('slug', s.get('url','')))}</a></td>
<td class="mono">recipe</td><td>{esc(s.get('licence','?'))}</td><td class="mono">curated pack</td>
<td class="note">{esc(plain(s.get('licence_hint',''))[:400])}</td></tr>''')
    g = graded.get(c["slug"])
    if g:
        qs = "\n".join(f'''<div class="g7q {'pass' if (q.get('verdict') or q.get('grade')) in ('relevant','partial','pass') else 'fail'}">
<b>Q:</b> {esc(q['q'])}<br><b>expect:</b> <span class="note">{esc(q.get('expect',''))}</span><br>
<b>top-5:</b> <span class="note mono">{esc(' / '.join(q.get('top5', [])[:5]))}</span><br>
<b>grade:</b> {esc(str(q.get('verdict') or q.get('grade','')))}</div>''' for q in g["queries"])
        g7sec = f'<section class="detail" id="g7"><h2>Retrieval spot-check (G7)</h2><p class="note">{g.get("median","?")}/5 relevant. Graded {esc(g.get("graded_at",""))}.</p>{qs}</section>'
    elif c["slug"] in g7:
        e = g7[c["slug"]]
        g7sec = f'''<section class="detail" id="g7"><h2>Retrieval spot-check (G7)</h2>
<p><b>{e["median"]:.1f}/5</b> median relevant ({esc(e["wave"])} stratified sample). Verdict: <b>{esc(e["verdict"])}</b>.</p></section>'''
    else:
        g7sec = '''<section class="detail" id="g7"><h2>Retrieval spot-check (G7)</h2>
<p class="note">Not yet spot-checked. Queries are pre-registered in the registry before grading.</p></section>'''
    imp = impact.get(c["slug"])
    if imp and imp.get("status") == "measured":
        impsec = f'''<section class="detail" id="ab"><h2>Agent A/B (drift-anchored)</h2>
<table><tr><th>arm</th><th>score</th><th>drift tasks</th></tr>
<tr><td>bare (offline, honest-unknown)</td><td class="mono">{esc(imp["p"])}</td><td class="mono">{esc(imp.get("dp","n/a"))}</td></tr>
<tr><td>with this corpus</td><td class="mono">{esc(imp["x"])}</td><td class="mono">{esc(imp.get("dx","n/a"))}</td></tr></table>
<p class="note">snapshot {esc(prov.get("date",""))} from <code>{esc(prov.get("branch",""))}</code>; protocol: benchmarks/corpus-tasks/PROTOCOL.md</p></section>'''
    else:
        impsec = ""
    if c.get("merged_into"):
        useit = (f'<div class="kbox"><b>Withdrawn</b><br>Merged into '
                 f'<a href="{esc(c["merged_into"])}.html"><code>{esc(c["merged_into"])}</code></a>. '
                 f'Use that corpus.</div>')
    elif c["status"] == "live" and c["lane"] == "A":
        s = esc(c["slug"])
        copybtn = (f' <button class="copy" data-copy="{esc(install_prompt(c))}" '
                   f'title="Copy a prompt that asks this corpus with one line">COPY PROMPT</button>')
        useit = (f'<div class="kbox"><b>Use it</b>{copybtn}<br>'
                 f'<code>xerj "your question" --corpus {s}</code><br>'
                 f'<code>xerj ask --def SYMBOL --corpus {s}</code>'
                 f'<p class="note">One line, no clone and no index: XERJ fetches this corpus&rsquo;s '
                 f'signed, pre-indexed pack the first time, then answers. The answer names the '
                 f'corpus, its pin date and its licence. Use <code>--def</code> for an exact value, '
                 f'signature or default. Needs the <code>xerj</code> CLI; the copy prompt checks it, '
                 f'says what success looks like, and shows how to stop only the node you started. '
                 f'No shell: the MCP tool <code>xerj_hub_ask</code> with <code>corpus: "{s}"</code>, '
                 f'once the pack is on the machine (a tool never downloads one). '
                 f'Not sure this is the right corpus? '
                 f'<a href="../llms.txt">The catalogue</a> lists every one, a line each.</p></div>')
    else:
        # Nothing to ask: a pack exists only for a live, lane-A corpus.
        useit = (f'<div class="kbox"><b>Use it</b><br>'
                 f'<span class="note">This entry has no hub pack, so there is nothing to ask. '
                 f'<a href="../llms.txt">The catalogue</a> lists the corpora that can be asked.</span></div>')
    return f'''<main>
<p><a class="backlink" href="../index.html">&larr; all corpora</a></p>
<h1 class="slug">{esc(c['slug'])} <span class="dot {stcls}"></span></h1>
<p class="note">{esc(c['kind'])} · status <b>{st}</b> · domain <b>{esc(c['cat'])}</b> ({esc(cat_desc[0])}) · lane {esc(c['lane'])} · refresh {esc(c['refresh'])} · tags <b>{esc(tags_txt)}</b></p>
<section class="detail"><h2>What an agent uses it for</h2><p>{esc(c['usecase'] or 'n/a')}</p></section>
{useit}
<section class="detail"><h2>Sources and pins</h2><table>
<tr><th>source</th><th>pin</th><th>licence</th><th>size</th><th>review note</th></tr>{''.join(rows)}</table>
<p class="note">{human_bytes(nbytes)} across {human_files(files)}. Added {esc(c['added'][:10] or 'n/a')}.</p></section>
<section class="detail"><h2>Rights</h2><p>{esc(' + '.join(licences) or '?')}.
{esc('Redistribution permitted with attribution per the licence review' if any(r.get('review',{}).get('use')=='adapt-with-attribution' for r in c['repos']) else 'See per-source review blocks')}.
The review block records a human opening each licence file at the pin. Detector output is a hint, never the verdict.</p></section>
{g7sec}{impsec}
</main>
{COPY_JS}'''


def category_body(k, cs, g7, graded, impact):
    title, what = CATS[k]
    cards = "\n".join(card(c, g7, graded, impact) for c in cs)
    n_live = sum(1 for c in cs if c["status"] == "live")
    return f'''<main>
<p><a class="backlink" href="../index.html">&larr; all domains</a></p>
<div class="kicker"><span class="accent">{esc(k)}</span><span class="dash">·</span><span>DOMAIN</span></div>
<h1 class="hero" style="font-size:clamp(56px,9vw,var(--fs-160))">{esc(k)}</h1>
<p class="lead">{esc(title)}. Agent asks: {esc(what)}. {n_live} live of {len(cs)} entries.</p>
<div class="grid">{cards}</div>
</main>'''


ABOUT = '''<main>
<div class="kicker"><span class="accent">ABOUT</span><span class="dash">·</span><span>THE HUB IN ONE PAGE</span></div>
<h1 class="hero" style="font-size:clamp(48px,7vw,120px)">WHY A<br><span class="accent">CORPUS HUB</span></h1>
<p class="lead">Agents answer from memory, and memory is stale, generic, or wrong on exactly
the questions that matter: what the regulation says at this pin, whether this CVE is
exploited, how this engine actually implements it. A corpus fixes that. The agent
downloads the domain it needs, searches it, and cites what it found. This hub is the
registry of those corpora: pinned, licence-checked, and measured.</p>
<section class="detail"><h2>Every corpus passes seven gates</h2>
<table>
<tr><th>gate</th><th>what satisfies it</th></tr>
<tr><td>G1 domain</td><td>the row finishes "an agent working on ___ would query this for ___" in one specific sentence</td></tr>
<tr><td>G2 shape</td><td>the retrieval unit matches the question: clause-level sections for standards, function bodies for code, advisory records for precedent</td></tr>
<tr><td>G3 un-memorisation</td><td>answer-bearing content is niche, internal, post-cutoff, or too detailed for recall. Famous-and-small is killed as retrieval theatre</td></tr>
<tr><td>G4 licence</td><td>a human opened the licence at the pin and wrote the review block. Detector output is a hint, never the verdict</td></tr>
<tr><td>G5 pin truth</td><td>manifest SHA equals the commit the build used. Rewrites are withdrawals, not re-pins</td></tr>
<tr><td>G6 registry validation</td><td>validate_hub.py green in CI on the corpus-hub branch</td></tr>
<tr><td>G7 retrieval spot-check</td><td>5 pre-registered domain queries, top-5 graded manually, median 3 or more relevant to pass</td></tr>
</table></section>
<section class="detail"><h2>Scores on the cards</h2>
<p><b>G7</b> is the retrieval spot-check above. <b>A/B</b> is the drift-anchored agent
benchmark: the same task run bare (offline, instructed to answer honestly rather than
fabricate) and with the corpus. The badge shows pass counts, bare then corpus.
A tie is a publishable outcome; the registry records it either way.</p></section>
<section class="detail"><h2>Tags</h2>
<p>Every corpus carries a region (US, UK, DE, EU, or GLOBAL) and topics. Filter the
front page by domain and region; free-text search covers topics and subtopics.</p></section>
<section class="detail"><h2>Rights policy</h2>
<p>Per-source licence verdicts live in each manifest's review block. PD (US government
works), OGL v3.0 (UK), and permissive licences ship as <code>adapt-with-attribution</code>.
GPL, AGPL, SSPL, and other restrictive sources are <code>approach-only</code>: readable
as design evidence, never copied. Closed families (ICC I-codes, Eurocodes, NEC, ASTM,
ISO, DIN) are documented as closed and excluded.</p></section>
<section class="detail"><h2>Contribute</h2>
<p>The registry is a git branch: <a href="https://github.com/xerj-org/xerj/tree/corpus-hub">corpus-hub</a>.
Read <a href="https://github.com/xerj-org/xerj/blob/corpus-hub/tools/xerj-code/hub/CONTRIBUTING.md">CONTRIBUTING.md</a>
and the program doc (PROGRAM-100.md). This site is generated from the registry by
<code>tools/corpus-hub-site/gen.py</code>. No hand-written corpus pages, ever.</p></section>
</main>'''


def contribute_prompt():
    """Paste-ready agent prompt: install XERJ, build a corpus from a folder or
    public repos, and open a PR to the corpus-hub registry."""
    return (
        "Help me contribute a corpus to the XERJ Corpus Hub (hub.xerj.org).\n"
        "1. If the xerj CLI is not installed, install it: curl -fsSL https://xerj.org/get | sh\n"
        "2. Build a corpus from a folder of documents I point you to: xerj autoindex <folder>\n"
        "   (or from public repos: xerj corpus add <name> <git-url> ...). This pins the sources\n"
        "   (commit SHA, files, bytes) and writes a manifest.\n"
        "3. Confirm the content is openly licensed and attributable. The hub is Apache-2.0 and\n"
        "   accepts only openly-licensed, source-cited corpora; record a review block per source\n"
        "   rather than trusting the licence detector.\n"
        "4. Fork github.com/xerj-org/xerj, copy the manifest to tools/xerj-code/hub/<name>.json,\n"
        "   add a one-line row to tools/xerj-code/hub/backlog/backlog-100.json, and open a pull\n"
        '   request targeting the corpus-hub branch. In the PR, say what the corpus is FOR.'
    )


def contribute_body():
    prompt = esc(contribute_prompt())
    return f'''<main>
<div class="kicker"><span class="accent">CONTRIBUTE</span><span class="dash">&middot;</span><span>ADD YOUR CORPUS</span></div>
<h1 class="hero" style="font-size:clamp(44px,6vw,var(--fs-96))">ADD YOUR<br><span class="accent">CORPUS</span></h1>
<p class="lead">Sharing data is like sharing code: it compounds. A corpus you contribute becomes
reference memory that any agent can search, so your domain knowledge makes everyone's agents
better, not just yours.</p>
<section class="detail"><h2>Contribute a corpus</h2>
<p class="note" style="margin-bottom:var(--sp-4)">Paste this into your coding agent. It explains itself.</p>
<div class="promptbox"><div class="pbh"><button class="copy" data-copy="{prompt}" title="Copy this prompt">COPY PROMPT</button></div>
<pre class="prompt">{prompt}</pre></div>
<p class="note">Full guide, templates and the review checklist:
<a href="https://github.com/xerj-org/xerj/blob/corpus-hub/tools/xerj-code/hub/CONTRIBUTING.md">CONTRIBUTING.md</a>.</p>
</section>
{COPY_JS}
</main>'''


def main():
    OUT.mkdir(exist_ok=True)
    (OUT / "assets").mkdir(exist_ok=True)
    (OUT / "corpus").mkdir(exist_ok=True)
    (OUT / "category").mkdir(exist_ok=True)
    (OUT / "assets" / "style.css").write_text(STYLE)
    corpora = load_registry()
    g7 = parse_waves_g7()
    graded = parse_graded_suites()
    impact, prov = load_impact()
    (OUT / "index.html").write_text(render_index(corpora, g7, graded, impact, prov))
    for c in corpora:
        (OUT / "corpus" / f"{c['slug']}.html").write_text(
            page(f"{c['slug']}: XERJ Corpus Hub", c["usecase"][:150], detail_body(c, g7, graded, impact, prov), rel="../"))
    for k, (title, what) in CATS.items():
        cs = [c for c in corpora if c["cat"] == k]
        (OUT / "category" / f"{k}.html").write_text(
            page(f"{k}: {title} domain", what, category_body(k, cs, g7, graded, impact), rel="../"))
    (OUT / "about.html").write_text(page("About: XERJ Corpus Hub", "How corpora are gated, scored, tagged, and licenced", ABOUT, rel=""))
    (OUT / "contribute.html").write_text(page("Contribute: XERJ Corpus Hub", "Share your knowledge: index a corpus and open a pull request", contribute_body(), rel=""))
    # llms.txt - agent-facing index of the registry, generated like everything
    # else. Short first screen (study rule 7), no obligation language, every
    # step verifiable; one line per live corpus.
    live = [c for c in corpora if c["status"] == "live"]
    imp2, _ = load_impact()
    lines = [
        "# XERJ Corpus Hub",
        "",
        f"> Reference memory for AI agents: {len(live)} pinned, licence-checked corpora.",
        "> Ask one question. XERJ picks the corpus, fetches its signed, pre-indexed pack the first",
        "> time, and answers on your own machine, instead of guessing from training or fighting web blocks.",
        "> This file is the agent index of https://hub.xerj.org, generated from the registry.",
        "> Registry of record (wins on any conflict with this file):",
        "> https://github.com/xerj-org/xerj/tree/corpus-hub/tools/xerj-code/hub",
        f"> Engine and install docs: https://xerj.org/llms.txt - Last updated: {STAMP}.",
        "",
        "## Hub corpus or local corpus?",
        "",
        "- Hub corpus: the answer is in public material you cannot quote exactly from",
        "  memory - a library's source at a pinned commit, a standard, a regulation, an",
        '  advisory feed. Find its slug under "Live corpora" below.',
        "- Local corpus: the answer is in files only this machine has - the project you",
        "  are working in, private documents, exports. Index them with",
        "  `xerj autoindex <folder> --prefix <name>` and query with",
        '  `xerj search --prefix <name> "<words>"` (without `--prefix` both use the default `ax`',
        "  prefix). Nothing is uploaded.",
        "- Neither: a fact you can already state exactly. Retrieving memorised content only adds cost.",
        "",
        "What you probably assume: a hub corpus must be cloned and indexed before you can ask it.",
        "What is true: it is built, signed and indexed ahead of time, and XERJ fetches it the first",
        "time you ask, checks its SHA-256 and signature, and answers on your machine. Nothing is",
        "cloned or indexed there, and the question is not sent anywhere.",
        "",
        "## Ask the hub (shell)",
        "",
        "Run in order. Each step says what success looks like.",
        "",
        '1. Check the CLI. Success: `xerj ask --help` prints `usage: xerj ask "<question>" [options]`.',
        "   Anything else: install or upgrade:",
        "",
        "       curl -fsSL https://xerj.org/get | sh",
        '       export PATH="$HOME/.local/bin:$PATH"',
        "",
        "2. Ask. No corpus name, no clone, no index:",
        "",
        '       xerj "your question"',
        "",
        '   (A question of one word needs `ask`: `xerj ask "word"`.)',
        "   Success: per hit, a header line and the passage; then the last line:",
        "",
        "       <corpus>  pin <date> (<N> days old)  <licence>  <file>:<line>",
        "       <the passage, or the declaration>",
        "       answered from: local pack",
        "",
        "   then any `warning:` lines. Cite the corpus, the pin date and the licence.",
        '   Pin one corpus with `--corpus <slug>` (slugs are under "Live corpora" below).',
        "",
        "3. An exact value, signature or default:",
        "",
        "       xerj ask --def <SYMBOL>",
        "",
        "   Success: the declaration, then a `value: <literal>` line for a constant or macro.",
        "   No passages.",
        "",
        "4. A large pack is not downloaded for you. The answer then ends",
        "   `run: xerj hub pull <slug> (<MB> MB)`:",
        "",
        "       xerj hub pull <slug>",
        "",
        "   Success: `pulled <slug> <pin8> <bytes> → <n> indices, <defs> definitions`. Then ask again.",
        "",
        "`no hub corpus matches` (exit 1): if a `closest corpora: ...` line follows, ask again with",
        "`--corpus <slug>`; without it the hub does not hold it, so say so instead of answering",
        "from memory. Exit 2: usage error or unknown corpus. Exit 3: a pack failed its checksum or",
        "signature check and was discarded. Exit 4: more than one corpus fits; the candidates print,",
        "run again with `--corpus <slug>`. Exit 5: this build has no hub public key, so nothing was",
        "downloaded; tell the user, a retry will not help.",
        "",
        "XERJ uses the node at `--url`, `$XERJ_URL` or localhost:9200 if one answers; otherwise it",
        "starts its own and records its PID. Stop only a node you started: `xerj hub stop` for",
        "XERJ's own (success: `stopped the hub node (pid <N>)`). Never `pkill xerj` or",
        "`killall xerj`: other XERJ nodes may be running on the machine and you would stop them too.",
        "If a pull fails with `not in limits.snapshot_repo_allowlist`, set",
        "`XERJ_URL=http://127.0.0.1:9399` (a loopback port nothing listens on) and ask again: XERJ",
        "then starts its own node, which can restore packs.",
        "",
        "Search is lexical (word overlap and BM25, not a neural model): ask in the words the source",
        "itself would use.",
        "",
        "## No shell",
        "",
        '- MCP: `xerj_hub_ask` takes `{"question": "your question"}` (optional `corpus`, `mode`);',
        '  `xerj_hub_def` takes `{"slug": "<slug>", "symbol": "<SYMBOL>"}`; `xerj_hub_find` takes',
        '  `{"question": "your question"}` and names the corpora that could answer.',
        "  A hub tool reads only packs already on the machine and never downloads one: when the pack",
        "  is missing it returns the `xerj hub pull <slug>` line for whoever has a shell to run once.",
        "- Register `xerj mcp` once with the binary's absolute path, then restart the session.",
        "  Success: `xerj_hub_ask` is in the tool list.",
        '  Claude Code: `claude mcp add --scope user xerj -- "$(command -v xerj)" mcp`',
        "  (`xerj init` writes `.mcp.json` and prints this line with the path filled in).",
        '  Cursor, Windsurf, Claude Desktop, Cline: root key `mcpServers`,',
        '  `{"mcpServers": {"xerj": {"command": "/abs/path/to/xerj", "args": ["mcp"]}}}`.',
        '  VS Code (`.vscode/mcp.json`): root key `servers`, and the entry needs `"type": "stdio"`;',
        "  a block under `mcpServers` is silently ignored.",
        "- HTTP only: hub.xerj.org serves pages and this catalogue, not answers. A node you can reach",
        "  that already holds the pack answers `POST <node>/xc-<slug>-*/_search` with",
        '  `{"query": {"multi_match": {"query": "<words>", "fields": ["body", "defs"]}}}`.',
        "",
        "## What to tell the person you work for",
        "",
        "Which corpus answered, its pin date and licence, and the file:line of every passage you relied on.",
        "",
        "## Scores on each line",
        "",
        "G7 = 5 pre-registered queries, graded on the top 5 results. A/B = the same agent on",
        "the same tasks without the corpus and with it (passes without -> passes with).",
        "",
        "## Live corpora",
        "",
    ]
    for c in live:
        files, nbytes, licences = corpus_stats(c)
        t = c.get("tags") or {}
        tags = "/".join(([t["region"]] if t.get("region") else []) + t.get("topics", []))
        g = graded.get(c["slug"])
        g7s = f"G7 {g['median']:.1f}/5" if g else (f"G7 {g7[c['slug']]['median']:.1f}/5" if c["slug"] in g7 else "G7 not yet graded")
        ab = ""
        r = imp2.get(c["slug"])
        if r and r.get("status") == "measured":
            ab = f"; agent A/B {r['p']} -> {r['x']}"
        lines.append(f"- [{c['slug']}](https://hub.xerj.org/corpus/{c['slug']}): {c['cat']} · {tags} · {'+'.join(licences) or '?'} · {human_bytes(nbytes)} · {g7s}{ab}; {plain(c['usecase'])[:160]}")
    lines += ["", "## Also in the registry", "",
              f"- {len(corpora) - len(live)} rows in candidate, planned, deferred, killed, or",
              "  withdrawn states (visible, not hidden: hub policy is honest statuses).",
              "  Browse all: https://hub.xerj.org"]
    (OUT / "llms.txt").write_text("\n".join(lines) + "\n")
    n = len(list((OUT / "corpus").glob("*.html")))
    print(f"generated: index, about, {len(CATS)} category pages, {n} corpus pages -> {OUT}")


if __name__ == "__main__":
    main()
