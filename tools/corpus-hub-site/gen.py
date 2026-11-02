#!/usr/bin/env python3
"""gen.py - generate the Corpus Hub static site (hub.xerj.org) from the registry.

Reads ONLY registry state on this branch (corpus-hub):
  tools/xerj-code/hub/*.json                 - manifests (lane A)
  tools/xerj-code/hub/backlog/backlog-100.json - categories/status/use-cases/tags
  tools/xerj-code/hub/WAVES.md               - G7 scores per wave
  tools/xerj-code/hub/backlog/*-graded.json  - graded G7 suites (non-IT lane)
  tools/xerj-code/hub/backlog/impact-snapshot-*.json - benchmark A/B snapshot
  tools/packs/*/recipe.toml                  - lane B packs
Writes out/ (index, per-corpus, per-category, whats-this, style, data.json).

Design: xerj.org brandbook, day palette only (user directive 2026-10-06:
strictly main-site fonts and CSS, domains as large typography, white default).
Copy: plain short sentences, no em-dashes, no filler intros (user directive
2026-10-06). Stdlib only; deterministic; no network. Rerun + redeploy on
registry change.
"""
import datetime as dt
import hashlib
import agent_score
import contributors
import gh_stars
import search_index
import os
import private_data
import repo_pages
import struct
import usage
import html
import json
import pathlib
import re
import tomllib
import zlib

ROOT = pathlib.Path(__file__).resolve().parents[2]
HUB = ROOT / "tools" / "xerj-code" / "hub"
PACKS = ROOT / "tools" / "packs"
OUT = pathlib.Path(__file__).resolve().parent / "out"
BRAND = pathlib.Path(__file__).resolve().parent / "brand"

# Canonical origin. xerj-hub.pages.dev serves the same files; every page names
# this origin so the pages.dev hostname is not a second site.
SITE = "https://hub.xerj.org"
PAGES_DEV_HOST = "xerj-hub.pages.dev"
GA_ID = "G-Z77ETCDJYL"
# Day-palette page background (--z-bg). The OG card is this colour with the
# light wordmark centered on it.
BG_RGB = (0xFA, 0xFA, 0xF7)
OG_W, OG_H = 1200, 630
OG_IMAGE = f"{SITE}/assets/og.png"
LOGO_IMAGE = f"{SITE}/assets/hub-logo.png"
ORG_ID = f"{SITE}/#organization"
SITE_ID = f"{SITE}/#website"

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

def build_stamp():
    """The date llms.txt and the search index say they were updated.

    HUB_STAMP (YYYY-MM-DD) wins. Otherwise the UTC date of the newest commit
    that touched the registry or this generator, so a rebuild of the same
    commit prints the same date. Falls back to today (UTC) outside git."""
    env = os.environ.get("HUB_STAMP", "")
    if re.match(r"^\d{4}-\d{2}-\d{2}$", env):
        return env
    try:
        import subprocess
        ts = subprocess.run(["git", "-C", str(ROOT), "log", "-1", "--format=%ct", "--",
                             "tools/xerj-code/hub", "tools/corpus-hub-site"],
                            capture_output=True, text=True, timeout=30).stdout.strip()
        if ts.isdigit():
            return dt.datetime.fromtimestamp(int(ts), dt.timezone.utc).strftime("%Y-%m-%d")
    except (OSError, ValueError):
        pass
    return dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d")


STAMP = build_stamp()

# IndexNow ownership key (scripts/seo/indexnow_ping.py on main). Public by
# design: IndexNow fetches https://hub.xerj.org/<key>.txt to check the host.
INDEXNOW_KEY = "e94e889e7bee019e40a7617324e7a20a"


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


def upstream_ids(c):
    """GitHub `owner/name` values this corpus actually pins, first-seen, at most four.

    Read from the manifest URL (and a repo field that is already `owner/name`).
    A bare repo name is not an owner and is left out.
    """
    ids = []
    for r in c.get("repos") or []:
        u = search_index.github_owner_repo(r.get("url", ""), r.get("repo", ""))
        if u and u not in ids:
            ids.append(u)
    for s in c.get("sources") or []:
        u = search_index.github_owner_repo(s.get("url", ""), s.get("repo", "") or s.get("slug", ""))
        if u and u not in ids:
            ids.append(u)
        if len(ids) == 4:
            break
    return ids[:4]


def upstream_label(c):
    return ", ".join(upstream_ids(c))


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


def card(c, g7, graded, impact, rel=""):
    """One corpus card. `rel` is the path back to the site root: category
    pages live one level down, and their cards linked category/corpus/<slug>
    (a 404) before it existed."""
    files, nbytes, licences = corpus_stats(c)
    st, stcls = STATUSES.get(c["status"], (c["status"], "mute"))
    up = upstream_label(c)
    repo_line = f'\n  <p class="crepo">{esc(up)}</p>' if up else ""
    return f'''<a class="card" data-cat="{esc(c['cat'])}" data-region="{esc(region(c))}" data-slug="{esc(c['slug'].lower())}" href="{rel}corpus/{esc(c['slug'])}">
  <div class="chead"><span class="cname">{esc(c['slug'])}</span><span class="dot {stcls}"></span></div>{repo_line}
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

# Which corpora have a signed hub pack to ask: the ones in the live signed
# catalogue (the same list `xerj ask` routes over). A live lane-A manifest whose
# pack has not been published yet gets no pull/ask commands, so the site never
# advertises a pack that does not exist. The site is generated before the
# deploy job fetches the catalogue, so read the live one here. If it cannot be
# read (offline build, XERJ_HUB_OFFLINE=1), fall back to treating every live
# lane-A corpus as packed, which is what the site printed before this gate.
CATALOGUE_JSON = os.environ.get("XERJ_HUB_CATALOGUE_JSON", SITE + "/api/v1/catalogue.json")
_PACKED = None


def packed_slugs():
    global _PACKED
    if _PACKED is None:
        _PACKED = False
        if os.environ.get("XERJ_HUB_OFFLINE") != "1":
            try:
                import urllib.request
                req = urllib.request.Request(CATALOGUE_JSON, headers={"User-Agent": "xerj-hub-site-gen"})
                with urllib.request.urlopen(req, timeout=20) as r:
                    cat = json.loads(r.read().decode("utf-8"))
                slugs = {c["slug"] for c in cat.get("corpora", []) if isinstance(c, dict) and c.get("slug")}
                if slugs:
                    _PACKED = slugs
                    print(f"gen: {len(slugs)} corpora have a hub pack (signed catalogue)")
            except Exception as e:  # noqa: BLE001 - any failure means "unknown", not "none"
                print(f"gen: catalogue unavailable ({e}); every live lane-A corpus is treated as packed")
    return _PACKED


def has_pack(c):
    """True when this live, lane-A corpus has a published hub pack (see above)."""
    if c["status"] != "live" or c["lane"] != "A":
        return False
    packed = packed_slugs()
    return packed is False or c["slug"] in packed


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
    up = upstream_label(c)
    return (
        f'Use the "{slug}" reference corpus from the XERJ Corpus Hub '
        f"(https://hub.xerj.org/corpus/{slug}) for this task."
        + (f" Upstream repo: {up}." if up else "")
        + (f" What it is for: {use}" if use else "") + "\n"
        "Run these in a shell, in order, and check each result before the next:\n"
        + CLI_STEP +
        f"2. Fetch the pack once: xerj hub pull {slug}\n"
        f"   Success: exit 0 and a line naming {slug} (or {slug}: already present). "
        "Exit 3 means the pack failed its SHA-256 or signature check: stop, do not use it.\n"
        f'3. Ask: xerj "<your question>" --corpus {slug}\n'
        f'   A question of one word needs ask: xerj ask "<word>" --corpus {slug}\n'
        + answer_lines(slug) +
        f"4. For an exact value, signature or default: xerj ask --def <SYMBOL> --corpus {slug}\n"
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
    def one(c):
        up = upstream_label(c)
        who = f"{up}, " if up else ""
        return f"- {c['slug']} ({who}{human_bytes(corpus_stats(c)[1])}): {plain(c.get('usecase') or '')[:110]}"
    listing = "\n".join(one(c) for c in live)
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
    up = upstream_label(c)
    if up:
        meta = f"{up} · {meta}"
    ttl = f'{c["slug"]} · {meta}'
    return (f'<div class="tile" data-cat="{esc(c["cat"])}" '
            f'data-region="{esc(region(c))}" data-name="{esc(c["slug"])}" '
            f'data-bytes="{nbytes}" title="{esc(ttl)}">'
            f'<span class="tdot {stcls}" title="{esc(st)}"></span>'
            f'<a class="tlink" href="corpus/{esc(c["slug"])}">'
            f'<span class="tn">{esc(c["slug"])}</span>'
            f'<span class="ts">{esc(meta)}</span></a></div>')


# Brandbook tokens from xerj.org (landing/style.css), day palette only.
# White schema is the one scheme: no dark variant, no prefers-color-scheme
# override (user directive 2026-10-06). 2026-10 readability pass: every text
# colour is >= 7:1 against --z-bg (ratios in the comments), body 18px / 1.6.
STYLE = """/* corpus hub - xerj.org brandbook, day palette, white only */
:root{
color-scheme:light;
--z-bg:#fafaf7;--z-ink:#1a1a1a;--z-mute:#4a4843;--z-faint:#e7e5de;--z-line:#d4d1c7;
--z-accent:#6b4400;--z-cmp:#3b536f;
--z-brand:#ffc400;--z-focus:#8a5a00;--z-code-bg:#f3f4f6;--z-code-line:#e2e4e8;
--ok:#1e5c30;--bad:#8f2427;--warn:#5f4a0e;
/* vs --z-bg: ink 16.6, mute 8.7, accent 8.2, cmp 7.6, ok 7.6, bad 8.2, warn 8.1 */
--font-display:'Big Shoulders Display','Inter',system-ui,sans-serif;
--font-prose:'Inter',system-ui,sans-serif;
--font-data:'IBM Plex Sans','Inter',system-ui,sans-serif;
--font-mono:'JetBrains Mono','IBM Plex Mono',monospace;
--fs-11:12px;--fs-13:14px;--fs-16:18px;--fs-20:20px;--fs-32:32px;
--fs-56:56px;--fs-96:96px;--fs-160:160px;
--track-ui:.14em;--track-label:.08em;
--sp-1:4px;--sp-2:8px;--sp-3:12px;--sp-4:16px;--sp-5:20px;--sp-6:24px;
--sp-8:32px;--sp-10:48px;--sp-12:64px;--sp-16:96px;
--page-x:64px;--max-w:1680px;
}
*{box-sizing:border-box}
body{margin:0;font-family:var(--font-prose);background:var(--z-bg);color:var(--z-ink);
line-height:1.6;font-size:var(--fs-16)}
a{color:inherit}
:where(p,li,dd,td,.kbox,.note) a:not([class]){color:var(--z-accent);text-decoration:underline;
text-decoration-thickness:1px;text-underline-offset:.18em}
:where(p,li,dd,td,.kbox,.note) a:not([class]):hover{color:var(--z-ink);text-decoration-thickness:2px}
:focus-visible{outline:3px solid var(--z-focus);outline-offset:2px}
::placeholder{color:var(--z-mute);opacity:1}
@media (max-width:720px){:root{--fs-16:17px}}
@media (prefers-reduced-motion:reduce){*,*::before,*::after{animation-duration:.01ms!important;
animation-iteration-count:1!important;transition-duration:.01ms!important}}
code{font-family:var(--font-mono);font-size:.9em;background:var(--z-code-bg);
border:1px solid var(--z-code-line);padding:.05em .35em}
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
/* sign-in (functions/auth/*): hidden until /api/me says enabled; see AUTH_JS */
.authbox[hidden]{display:none}
.authbox{position:relative;align-self:center}
.authbox summary{list-style:none;cursor:pointer;color:var(--z-mute);display:inline-flex;align-items:center;gap:var(--sp-2)}
.authbox summary::-webkit-details-marker{display:none}
.authbox summary:hover{color:var(--z-ink)}
.authbox summary img{width:24px;height:24px;border-radius:50%;border:1px solid var(--z-line);display:block}
.authbox .menu{position:absolute;right:0;top:calc(100% + 8px);min-width:220px;background:#fff;border:1px solid var(--z-line);
padding:var(--sp-2) 0;display:flex;flex-direction:column;z-index:60;box-shadow:0 6px 24px rgba(0,0,0,.06)}
.authbox .menu a,.authbox .menu span{padding:var(--sp-2) var(--sp-4);display:block}
.authbox .menu span{color:var(--z-ink);text-transform:none;letter-spacing:normal;font-size:var(--fs-13)}
.authbox .menu a:hover{background:var(--z-faint);color:var(--z-ink)}
.mystars{width:100%;border-collapse:collapse;margin-top:var(--sp-6);background:#fff}
.mystars th,.mystars td{border:1px solid var(--z-line);padding:var(--sp-2) var(--sp-3);text-align:left}
button.cta{border:0;cursor:pointer}button.cta:disabled{opacity:.45;cursor:default}
.addrepos td:first-child{width:44px;text-align:center}.addrepos .why{color:var(--z-mute);font-size:var(--fs-13)}
.addrepos input,.addagree input{width:22px;height:22px;margin:0;accent-color:var(--z-accent);cursor:pointer;flex:none}
.addrepos tr.pick{cursor:pointer}.addrepos tr.pick:hover{background:var(--z-bg)}.addrepos tr.on{background:#fbf3e4}
.addbar{display:flex;flex-wrap:wrap;gap:var(--sp-3);align-items:center;margin-top:var(--sp-5)}
.addagree{display:flex;gap:var(--sp-3);align-items:flex-start;max-width:760px;margin-top:var(--sp-5);cursor:pointer}
.addgo{display:flex;flex-wrap:wrap;gap:var(--sp-3);align-items:center;margin-top:var(--sp-4)}.addgo .note{margin:0}
@media (max-width:600px){.addrepos .opt{display:none}}
.spark{width:160px;height:32px;display:block}.spark polyline{fill:none;stroke:var(--z-accent);stroke-width:1.5}
.authnote{position:fixed;bottom:var(--sp-5);right:var(--sp-5);background:var(--z-ink);color:#fff;padding:var(--sp-3) var(--sp-5);
font-family:var(--font-data);font-size:var(--fs-13);z-index:70}
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
.kicker .dash{margin:0 6px}
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
.statgrid{display:grid;grid-template-columns:minmax(0,1fr) minmax(0,1fr);grid-auto-rows:1fr;
gap:1px;background:var(--z-line);border:1px solid var(--z-line)}
.heroside .cta{margin-top:0;text-align:center}
.stat{background:var(--z-bg);padding:var(--sp-4) var(--sp-5);min-height:88px;
display:flex;flex-direction:column;justify-content:center;gap:4px}
.stat.wide{grid-column:1 / -1}
.stat a{display:flex;flex-direction:column;gap:4px;text-decoration:none;color:inherit}.stat a:hover b{color:var(--z-accent)}
.stat b{font-family:var(--font-display);font-weight:800;white-space:nowrap;
font-size:var(--fs-32);line-height:1;color:var(--z-ink)}
.stat span{font-family:var(--font-data);font-size:var(--fs-11);font-weight:500;
text-transform:uppercase;letter-spacing:var(--track-ui);color:var(--z-mute)}
/* full numbers (7,194,550) on a phone: shrink with the viewport instead of overflowing it */
@media (max-width:560px){.stat{padding:var(--sp-3) var(--sp-3);min-width:0}.stat b{font-size:clamp(17px,5.4vw,var(--fs-32))}}
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
.toolbar input{border-color:#8c8a82}
.toolbar input:focus{outline:3px solid var(--z-focus);outline-offset:1px;border-color:var(--z-accent)}
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
.crepo{font-family:var(--font-mono);font-size:var(--fs-11);color:var(--z-mute);margin:0}
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
.badge.g7.mute{border-style:dashed}
/* detail pages */
section.detail h2{font-family:var(--font-display);font-weight:700;font-size:var(--fs-32);
line-height:.95;border-bottom:1px solid var(--z-line);padding-bottom:var(--sp-2);margin:var(--sp-10) 0 var(--sp-4)}
table{border-collapse:collapse;width:100%;font-size:var(--fs-13);font-family:var(--font-prose)}
@media (max-width:600px){.detail table{display:block;overflow-x:auto}}
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

def href(path):
    """Internal link with no .html. Cloudflare Pages 308s /x.html to /x and
    /dir/index.html to /dir/, so the link has to be the URL that is served."""
    frag = ""
    if "#" in path:
        path, frag = path.split("#", 1)
        frag = "#" + frag
    query = ""
    if "?" in path:
        path, query = path.split("?", 1)
        query = "?" + query
    if path.endswith("/index.html"):
        path = path[:-len("index.html")]
    elif path == "index.html":
        path = "./"
    elif path.endswith(".html"):
        path = path[:-len(".html")]
    return path + query + frag


def canonical_url(path):
    """Absolute https://hub.xerj.org URL. path is already extensionless."""
    if not path.startswith("/"):
        path = "/" + path
    return SITE + path


def file_url(rel):
    """Output-relative HTML path -> the URL Cloudflare Pages serves."""
    rel = rel.replace("\\", "/")
    if rel == "index.html":
        return canonical_url("/")
    if rel.endswith("/index.html"):
        return canonical_url("/" + rel[:-len("index.html")])
    if rel.endswith(".html"):
        return canonical_url("/" + rel[:-len(".html")])
    return canonical_url("/" + rel)


def _png_chunk(tag, data):
    return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)


def read_png(path):
    """RGBA rows from a non-interlaced 8-bit PNG. Stdlib only."""
    data = pathlib.Path(path).read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path} is not a PNG")
    pos, w, h, idat = 8, None, None, b""
    while pos < len(data):
        n, typ = struct.unpack(">I4s", data[pos:pos + 8])
        pos += 8
        chunk, pos = data[pos:pos + n], pos + n + 4
        if typ == b"IHDR":
            w, h, bit, color, _comp, _filt, inter = struct.unpack(">IIBBBBB", chunk[:13])
            if bit != 8 or color != 6 or inter != 0:
                raise ValueError(f"{path} is not 8-bit non-interlaced RGBA")
        elif typ == b"IDAT":
            idat += chunk
        elif typ == b"IEND":
            break
    raw = zlib.decompress(idat)
    bpp, stride, rows, i, prev = 4, w * 4, [], 0, bytearray(w * 4)
    for _y in range(h):
        f, i = raw[i], i + 1
        row = bytearray(raw[i:i + stride])
        i += stride
        if f == 1:
            for x in range(stride):
                row[x] = (row[x] + (row[x - bpp] if x >= bpp else 0)) & 255
        elif f == 2:
            for x in range(stride):
                row[x] = (row[x] + prev[x]) & 255
        elif f == 3:
            for x in range(stride):
                left = row[x - bpp] if x >= bpp else 0
                row[x] = (row[x] + ((left + prev[x]) // 2)) & 255
        elif f == 4:
            for x in range(stride):
                a = row[x - bpp] if x >= bpp else 0
                b, c = prev[x], prev[x - bpp] if x >= bpp else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pred = a if pa <= pb and pa <= pc else b if pb <= pc else c
                row[x] = (row[x] + pred) & 255
        elif f != 0:
            raise ValueError(f"unsupported PNG filter {f}")
        rows.append(row)
        prev = row
    return w, h, rows


def _composite(dst, src):
    """src over dst, both (r, g, b, a) bytes."""
    sa, da = src[3] / 255.0, dst[3] / 255.0
    out_a = sa + da * (1 - sa)
    if out_a <= 0:
        return (0, 0, 0, 0)
    ch = []
    for i in range(3):
        ch.append(round((src[i] / 255.0 * sa + dst[i] / 255.0 * da * (1 - sa)) / out_a * 255))
    ch.append(round(out_a * 255))
    return tuple(ch)


def _flatten(rows, bg=BG_RGB):
    """RGBA rows -> tightly packed RGBA bytes on an opaque background."""
    h, w = len(rows), len(rows[0]) // 4
    out = bytearray(w * h * 4)
    base = (bg[0], bg[1], bg[2], 255)
    i = 0
    for row in rows:
        for x in range(0, w * 4, 4):
            px = _composite(base, tuple(row[x:x + 4]))
            out[i:i + 4] = bytes(px)
            i += 4
    return w, h, bytes(out)


def write_png(path, w, h, rgba):
    raw = bytearray()
    stride = w * 4
    for y in range(h):
        raw.append(0)
        raw += rgba[y * stride:(y + 1) * stride]
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
    png = (b"\x89PNG\r\n\x1a\n" + _png_chunk(b"IHDR", ihdr)
           + _png_chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + _png_chunk(b"IEND", b""))
    pathlib.Path(path).write_bytes(png)


def write_brand_assets(out):
    """Favicon plus a light social card.

    The mark and the wordmark are the light Corpus Hub logos from the
    brand-assets branch (hub-logo-128.png, hub-logo-text-512.png): black and
    gold on a transparent background. Social cards composite the wordmark onto
    the hub's own page background so a transparent corner is not black.
    """
    out = pathlib.Path(out)
    (out / "assets").mkdir(parents=True, exist_ok=True)
    mark = (BRAND / "hub-logo-128.png").read_bytes()
    (out / "favicon.png").write_bytes(mark)
    _w, _h, rows = read_png(BRAND / "hub-logo-text-512.png")
    lw, lh, flat = _flatten(rows)
    write_png(out / "assets" / "hub-logo.png", lw, lh, flat)
    canvas = bytearray(OG_W * OG_H * 4)
    pix = bytes(BG_RGB) + b"\xff"
    for i in range(0, len(canvas), 4):
        canvas[i:i + 4] = pix
    ox, oy = (OG_W - lw) // 2, (OG_H - lh) // 2
    for y in range(lh):
        src = y * lw * 4
        dst = ((oy + y) * OG_W + ox) * 4
        canvas[dst:dst + lw * 4] = flat[src:src + lw * 4]
    write_png(out / "assets" / "og.png", OG_W, OG_H, bytes(canvas))


def _ld(obj):
    body = json.dumps(obj, ensure_ascii=False, indent=2).replace("<", "\\u003c")
    return f'<script type="application/ld+json">\n{body}\n</script>'


def seo_block(title, desc, path, rel, kind="WebPage", image=None, noindex=False,
              og_title=None, og_desc=None, ld_extra=None):
    """Canonical, social tags, JSON-LD, favicon, analytics. Not visible layout."""
    url = canonical_url(path)
    ot, od = og_title if og_title is not None else title, og_desc if og_desc is not None else desc
    own_image = image is None
    img = OG_IMAGE if own_image else image
    card = "summary_large_image" if own_image else "summary"
    icon = f"{rel}favicon.png"
    lines = [
        f'<link rel="canonical" href="{url}">',
        f'<link rel="icon" href="{icon}" type="image/png" sizes="128x128">',
        f'<meta property="og:site_name" content="XERJ Corpus Hub">',
        f'<meta property="og:type" content="website">',
        f'<meta property="og:title" content="{esc(ot)}">',
        f'<meta property="og:description" content="{esc(od)}">',
        f'<meta property="og:url" content="{url}">',
        f'<meta property="og:image" content="{esc(img)}">',
    ]
    if own_image:
        lines += [
            f'<meta property="og:image:width" content="{OG_W}">',
            f'<meta property="og:image:height" content="{OG_H}">',
            f'<meta property="og:image:alt" content="{esc(ot)}">',
        ]
    lines += [
        '<meta property="og:locale" content="en_US">',
        f'<meta name="twitter:card" content="{card}">',
        f'<meta name="twitter:title" content="{esc(ot)}">',
        f'<meta name="twitter:description" content="{esc(od)}">',
        f'<meta name="twitter:image" content="{esc(img)}">',
    ]
    if noindex:
        lines.append('<meta name="robots" content="noindex,follow">')
    # Same HTML is served on the pages.dev hostname. Leave that host before
    # analytics runs. Branch preview hostnames (*.pages.dev) stay put so a
    # deploy preview still opens.
    lines.append(
        '<script>if(location.hostname==="' + PAGES_DEV_HOST + '")'
        'location.replace("' + SITE + '"+location.pathname+location.search+location.hash);</script>'
    )
    lines += [
        _ld({
            "@context": "https://schema.org",
            "@type": "Organization",
            "@id": ORG_ID,
            "name": "XERJ",
            "url": "https://xerj.org/",
            "logo": LOGO_IMAGE,
            "sameAs": ["https://github.com/xerj-org/xerj", "https://x.com/xerj_org"],
        }),
        _ld({
            "@context": "https://schema.org",
            "@type": "WebSite",
            "@id": SITE_ID,
            "name": "XERJ Corpus Hub",
            "url": SITE + "/",
            "inLanguage": "en",
            "publisher": {"@id": ORG_ID},
        }),
        _ld({
            "@context": "https://schema.org",
            "@type": kind,
            "name": plain(ot),
            "description": plain(od),
            "url": url,
            "inLanguage": "en",
            "isPartOf": {"@id": SITE_ID},
            "publisher": {"@id": ORG_ID},
            **(ld_extra or {}),
        }),
        f'<script async src="https://www.googletagmanager.com/gtag/js?id={GA_ID}"></script>',
        "<script>window.dataLayer=window.dataLayer||[];function gtag(){dataLayer.push(arguments);}"
        f"gtag('js',new Date());gtag('config','{GA_ID}');</script>",
    ]
    return "\n".join(lines)


# Allows every crawler, including the AI crawlers a managed robots.txt would
# otherwise name. Content-Signal tokens are the three at contentsignals.org.
ROBOTS = """# hub.xerj.org. Every crawler may fetch every page; sign-in and API
# endpoints are not pages.
# Agent index: https://hub.xerj.org/llms.txt

User-agent: *
Content-Signal: search=yes, ai-input=yes, ai-train=yes
Allow: /
Disallow: /auth/
Disallow: /api/

# Repeated so a crawler that only honours its own group still sees the rules.
# These are the user-agents Cloudflare's managed robots.txt names, plus the
# OpenAI and Perplexity crawlers.
""" + "".join(f"User-agent: {ua}\nAllow: /\nDisallow: /auth/\nDisallow: /api/\n\n" for ua in (
    "GPTBot", "ChatGPT-User", "OAI-SearchBot", "Google-Extended", "Applebot-Extended", "ClaudeBot",
    "CCBot", "Amazonbot", "Bytespider", "meta-externalagent", "PerplexityBot")) + """Sitemap: https://hub.xerj.org/sitemap.xml
"""


SITEMAPS = (("pages", "home, categories and guides"), ("corpora", "registry corpora"),
            ("repos", "repo pages"), ("contributors", "contributor pages"))


def sitemap_kind(rel, repo_pages_set=frozenset()):
    if rel.startswith("corpus/"):
        return "repos" if rel in repo_pages_set else "corpora"
    if rel.startswith("contributors/"):
        return "contributors"
    return "pages"


def write_sitemap(out, lastmod=None, repo_set=frozenset()):
    """A sitemap index at /sitemap.xml and one sitemap per page type. One
    <loc> per indexable HTML page, identical to that page's canonical.
    <lastmod> only where the page has a real date (a consent, a clone or a
    review date, or the day its GitHub stats were fetched); never the build
    day. No priority, no changefreq."""
    import xml.sax.saxutils as saxutils
    out = pathlib.Path(out)
    lastmod = lastmod or {}
    groups = {k: [] for k, _ in SITEMAPS}
    for f in sorted(out.rglob("*.html")):
        rel = f.relative_to(out).as_posix()
        if rel == "404.html":
            continue
        head = f.read_text(encoding="utf-8").split("</head>", 1)[0]
        if 'name="robots"' in head and "noindex" in head:
            continue
        groups[sitemap_kind(rel, repo_set)].append((file_url(rel), lastmod.get(rel, "")))
    index = ['<?xml version="1.0" encoding="UTF-8"?>',
             '<sitemapindex xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">']
    total = 0
    for k, _ in SITEMAPS:
        rows = sorted(groups[k])
        if not rows:
            continue
        total += len(rows)
        lines = ['<?xml version="1.0" encoding="UTF-8"?>',
                 '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">']
        for loc, lm in rows:
            lines.append("  <url>")
            lines.append(f"    <loc>{saxutils.escape(loc)}</loc>")
            if lm:
                lines.append(f"    <lastmod>{lm}</lastmod>")
            lines.append("  </url>")
        lines.append("</urlset>")
        (out / f"sitemap-{k}.xml").write_text("\n".join(lines) + "\n", encoding="utf-8")
        newest = max((lm for _, lm in rows if lm), default="")
        index += ["  <sitemap>", f"    <loc>{SITE}/sitemap-{k}.xml</loc>"]
        if newest:
            index.append(f"    <lastmod>{newest}</lastmod>")
        index.append("  </sitemap>")
    index.append("</sitemapindex>")
    (out / "sitemap.xml").write_text("\n".join(index) + "\n", encoding="utf-8")
    return total


FONTS = """<link rel="preconnect" href="https://fonts.googleapis.com">
<link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
<link href="https://fonts.googleapis.com/css2?family=Big+Shoulders+Display:wght@400;700;800;900&family=Inter:wght@400;500;600;700&family=IBM+Plex+Sans:wght@400;500;600;700&family=JetBrains+Mono:wght@400;500;700&display=swap" rel="stylesheet">"""

PAGE = """<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title}</title><meta name="description" content="{desc}">
{seo}
{fonts}
<link rel="stylesheet" href="{rel}assets/style.css?v={ver}"></head>
<body><nav class="nav"><a class="brand" href="https://xerj.org">XERJ<b>·</b>HUB</a>
<a href="{home}">CORPORA</a>
<a href="{people}">CONTRIBUTORS</a>
<a href="{rel}whats-this/">WHAT'S THIS?</a>
<a href="{rel}llms.txt" title="The catalogue: every live corpus, one line each">CATALOGUE</a>
<span class="spacer"></span>{search}<details class="authbox" id="hubauth" hidden></details><a class="navicon" href="https://x.com/xerj_org" title="XERJ on X" target="_blank" rel="noopener"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M18.244 2.25h3.308l-7.227 8.26 8.502 11.24H16.17l-5.214-6.817L4.99 21.75H1.68l7.73-8.835L1.254 2.25H8.08l4.713 6.231zm-1.161 17.52h1.833L7.084 4.126H5.117z"/></svg></a><a class="navicon" href="https://discord.com/invite/yetXFusXfs" title="XERJ on Discord" target="_blank" rel="noopener"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M20.317 4.37a19.79 19.79 0 00-4.885-1.515.074.074 0 00-.079.037c-.21.375-.444.864-.608 1.25a18.27 18.27 0 00-5.487 0 12.64 12.64 0 00-.617-1.25.077.077 0 00-.079-.037A19.74 19.74 0 003.677 4.37a.07.07 0 00-.032.027C.533 9.046-.32 13.58.099 18.057a.082.082 0 00.031.057 19.9 19.9 0 005.993 3.03.078.078 0 00.084-.028c.462-.63.874-1.295 1.226-1.994a.076.076 0 00-.041-.106 13.1 13.1 0 01-1.872-.892.077.077 0 01-.008-.128 10.2 10.2 0 00.372-.292.074.074 0 01.077-.01c3.928 1.793 8.18 1.793 12.062 0a.074.074 0 01.078.01c.12.098.246.198.373.292a.077.077 0 01-.006.127 12.3 12.3 0 01-1.873.892.077.077 0 00-.041.107c.36.698.772 1.362 1.225 1.993a.076.076 0 00.084.028 19.84 19.84 0 006.002-3.03.077.077 0 00.032-.054c.5-5.177-.838-9.674-3.549-13.66a.061.061 0 00-.031-.03zM8.02 15.33c-1.183 0-2.157-1.085-2.157-2.419 0-1.333.956-2.419 2.157-2.419 1.21 0 2.176 1.096 2.157 2.42 0 1.333-.956 2.418-2.157 2.418zm7.975 0c-1.183 0-2.157-1.085-2.157-2.419 0-1.333.955-2.419 2.157-2.419 1.21 0 2.176 1.096 2.157 2.42 0 1.333-.946 2.418-2.157 2.418z"/></svg></a>{ghstars}<a href="https://xerj.org">XERJ.ORG</a><a href="{add}" class="navcta">ADD YOUR CORPUS</a></nav>
{body}
<footer><span>XERJ · CORPUS HUB · {stamp}</span>
<span>PINNED SOURCES · VERIFIED LICENCES · MEASURED RETRIEVAL</span>
<span><a href="{home}">CORPORA</a> · <a href="{rel}whats-this/">WHAT'S THIS?</a> ·
<a href="{rel}llms.txt">LLMS.TXT</a></span></footer>
{navjs}{authjs}{starsjs}
<script src="{rel}assets/search.js?v={sver}" defer></script>
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
STYLE += contributors.STYLE
STYLE += repo_pages.STYLE
STYLE += """
/* What's this? (WHATS_THIS): short, readable measure */
.whats .lead,.whats .detail{max-width:72ch}
.steps{padding-left:1.4em}.steps li{margin:0 0 var(--sp-4)}
"""
STYLE += usage.STYLE
STYLE += agent_score.STYLE
STYLE += search_index.STYLE
STYLE += gh_stars.STYLE
SEARCH_VER = hashlib.sha1(search_index.JS.encode("utf-8")).hexdigest()[:8]
STYLE_VER = hashlib.sha1(STYLE.encode("utf-8")).hexdigest()[:8]
# GitHub stars badge for the nav, from the count saved in data/xerj_stars.json.
GH_STARS_BADGE = gh_stars.badge()


# Sign in / avatar / sign out in the nav. Asks /api/me (functions/api/me.js); the
# box stays hidden unless sign-in is enabled (HUB_SESSION_SECRET and at least one
# provider's OAuth app configured), and if the Function is absent or errors.
# Only offered providers are listed. window.hubMe is the one shared /api/me request
# (whichever page script runs first starts it). User text goes in via textContent, never as HTML.
AUTH_JS = """<script>
(function(){
  var box=document.getElementById('hubauth');
  function el(t,a,txt){var e=document.createElement(t);for(var k in a)e.setAttribute(k,a[k]);if(txt)e.textContent=txt;return e}
  var here=encodeURIComponent(location.pathname+location.search.replace(/([?&])signin=[^&]*&?/,'$1').replace(/[?&]$/,''));
  var m=location.search.match(/[?&]signin=(failed|cancelled|disabled)/);
  if(m){
    var n=el('div',{'class':'authnote',role:'status'},m[1]==='failed'?'Sign-in did not complete. Try again.':m[1]==='cancelled'?'Sign-in cancelled.':'Sign-in is not available yet.');
    document.body.appendChild(n);setTimeout(function(){n.remove()},5000);
    history.replaceState(null,'',decodeURIComponent(here)+location.hash);
  }
  (window.hubMe=window.hubMe||fetch('/api/me',{credentials:'same-origin'}).then(function(r){return r.ok?r.json():null}).catch(function(){return null})).then(function(me){
    if(!box||!me||!me.ok||!me.enabled)return;
    var sum=el('summary'),menu=el('div',{'class':'menu'});
    if(me.user){
      var u=me.user;
      if(u.avatar)sum.appendChild(el('img',{src:u.avatar,alt:'',referrerpolicy:'no-referrer'}));
      sum.appendChild(el('span',{},u.login));
      menu.appendChild(el('span',{},(u.name||u.login)+' · '+(u.provider==='hf'?'Hugging Face':'GitHub')));
      (me.links||[]).forEach(function(l){menu.appendChild(el('a',{href:l.href},l.label))});
      menu.appendChild(el('a',{href:'/auth/logout?return='+here,rel:'nofollow'},'Sign out'));
    }else{
      sum.textContent='Sign in';
      if(me.providers.github)menu.appendChild(el('a',{href:'/auth/github/login?return='+here,rel:'nofollow'},'with GitHub'));
      if(me.providers.hf)menu.appendChild(el('a',{href:'/auth/hf/login?return='+here,rel:'nofollow'},'with Hugging Face'));
    }
    box.appendChild(sum);box.appendChild(menu);box.hidden=false;
    document.addEventListener('click',function(e){if(!box.contains(e.target))box.open=false});
  });
})();
</script>"""


# /me.html: the signed-in visitor's own hub page. The static file is an empty
# shell; everything on it comes from the authenticated API at view time
# (/api/v1/me/stars; functions/api/v1/me/stars.js), so nothing personal is ever
# in the generated site.
ME_BODY = """<main class="cwrap"><section class="whead"><h1>Your hub</h1>
<p id="me-lead">Sign in from the header to see the corpora and contributors you starred, and how their hub stars grew.</p></section>
<div id="me-stars"></div></main>"""

ME_JS = """<script>
(function(){
  var lead=document.getElementById('me-lead'),box=document.getElementById('me-stars');
  function el(t,a,txt){var e=document.createElement(t);for(var k in a)e.setAttribute(k,a[k]);if(txt!=null)e.textContent=txt;return e}
  function spark(h){ // cumulative hub stars over the days we have history for
    if(!h||!h.length)return null;
    var c=0,pts=h.map(function(d){c+=d[1];return c}),mx=pts[pts.length-1]||1,w=160,ht=32,n=pts.length;
    var d=pts.map(function(v,i){return (n===1?w:i*w/(n-1)).toFixed(1)+','+(ht-v/mx*(ht-2)-1).toFixed(1)}).join(' ');
    if(n===1)d='0,'+(ht-1)+' '+d;
    var s=document.createElementNS('http://www.w3.org/2000/svg','svg');s.setAttribute('viewBox','0 0 '+w+' '+ht);s.setAttribute('class','spark');
    var p=document.createElementNS('http://www.w3.org/2000/svg','polyline');p.setAttribute('points',d);s.appendChild(p);
    var t=document.createElementNS('http://www.w3.org/2000/svg','title');t.textContent='+'+c+' hub stars from '+h[0][0]+' to '+h[h.length-1][0];s.appendChild(t);
    return s;
  }
  (window.hubMe=window.hubMe||fetch('/api/me',{credentials:'same-origin'}).then(function(r){return r.ok?r.json():null}).catch(function(){return null})).then(function(me){
    if(!me||!me.enabled){lead.textContent='Sign-in is not enabled on this hub yet.';return}
    if(!me.user)return;
    lead.textContent='Signed in as '+me.user.login+'. Everything you starred on the hub, newest first.';
    fetch('/api/v1/me/stars',{credentials:'same-origin'}).then(function(r){return r.json()}).then(function(r){
      if(!r||!r.ok){lead.textContent='Could not load your stars right now.';return}
      if(!r.enabled){lead.textContent='Hub stars are not enabled on this hub yet.';return}
      if(!r.stars.length){box.appendChild(el('p',{'class':'note'},'No stars yet. Star a corpus or a contributor from its page.'));return}
      var t=el('table',{'class':'mystars'}),hd=el('tr');
      ['','starred','hub stars','since daily history began'].forEach(function(x){hd.appendChild(el('th',{},x))});t.appendChild(hd);
      r.stars.forEach(function(s){
        var tr=el('tr'),a=el('td');a.appendChild(el('a',{href:s.href},(s.kind==='corpus'?'corpus ':'@')+s.id));tr.appendChild(a);
        tr.appendChild(el('td',{'class':'mono'},(s.starred_at||'').slice(0,10)));
        tr.appendChild(el('td',{'class':'mono'},'\u2605 '+Number(s.count||0).toLocaleString('en-US')));
        var h=el('td'),sp=spark(s.history);if(sp)h.appendChild(sp);else h.textContent=s.history?'no new stars yet':'';tr.appendChild(h);
        t.appendChild(tr);
      });
      box.appendChild(t);
    }).catch(function(){lead.textContent='Could not load your stars right now.'});
  });
})();
</script>"""


# /add: a signed-in GitHub owner picks their own public repositories and adds them to
# the hub (user, 2026-10-27: "github auth then show repos they own, add to corpus hub
# entirely"). The repository list comes from the public GitHub API in the visitor's
# browser; the choice and the agreement go to /api/v1/me/repos
# (functions/api/v1/me/repos.js), and the outreach pipeline checks ownership on
# GitHub before the 2-hourly publish. Static shell, like /me: nothing personal in the site.
ADD_BODY = """<main class="cwrap"><section class="whead"><h1>Add your repositories</h1>
<p id="add-lead">Sign in with GitHub to see the public repositories you own and add any of them to the Corpus Hub, with attribution to you.</p></section>
<div id="add-box"></div></main>"""

ADD_JS = """<script>
(function(){
  var lead=document.getElementById('add-lead'),box=document.getElementById('add-box');
  function el(t,a,txt){var e=document.createElement(t);for(var k in a)e.setAttribute(k,a[k]);if(txt!=null)e.textContent=txt;return e}
  function api(body){return fetch('/api/v1/me/repos',{method:'POST',credentials:'same-origin',headers:{'content-type':'application/json'},body:JSON.stringify(body)}).then(function(r){return r.json()})}
  function signin(){var a=el('a',{'class':'cta',href:'/auth/github/login?return=%2Fadd',rel:'nofollow'},'Sign in with GitHub');box.appendChild(a)}
  function gh(login,page,acc){ // public repos the account owns, newest push first; 60 requests/hour per browser
    return fetch('https://api.github.com/users/'+encodeURIComponent(login)+'/repos?type=owner&sort=pushed&per_page=100&page='+page,{headers:{accept:'application/vnd.github+json'}})
      .then(function(r){if(!r.ok)throw new Error(r.status===403||r.status===429?'limit':'gh');return r.json()})
      .then(function(rs){acc=acc.concat(rs);return rs.length===100&&page<10?gh(login,page+1,acc):acc});
  }
  (window.hubMe=window.hubMe||fetch('/api/me',{credentials:'same-origin'}).then(function(r){return r.ok?r.json():null}).catch(function(){return null})).then(function(me){
    if(!me||!me.enabled){lead.textContent='Sign-in is not enabled on this hub yet.';return}
    if(!me.user){signin();return}
    if(me.user.provider!=='github'){lead.textContent='You are signed in with Hugging Face. Repositories are added from a GitHub account.';signin();return}
    var login=me.user.login;
    lead.textContent='Signed in as '+login+'. Loading your public repositories\u2026';
    fetch('/api/v1/me/repos',{credentials:'same-origin'}).then(function(r){return r.json()}).then(function(st){
      if(!st||!st.ok){lead.textContent='Could not load this page right now. Try again in a minute.';return}
      if(!st.enabled){lead.textContent='Adding repositories is not enabled on this hub yet.';return}
      var req={},hub={};
      st.requested.forEach(function(r){req[r.repo.toLowerCase()]=r});
      st.on_hub.forEach(function(r){hub[r.toLowerCase()]=1});
      return gh(login,1,[]).then(function(repos){render(st,repos,req,hub)},function(e){
        lead.textContent=e.message==='limit'?'GitHub limits how often a browser can list repositories. Try again in an hour.':'Could not list your repositories on GitHub right now. Try again in a minute.';
      });
    }).catch(function(){lead.textContent='Could not load this page right now. Try again in a minute.'});

    function render(st,repos,req,hub){
      repos=repos.filter(function(r){return !r.private&&r.owner&&r.owner.login.toLowerCase()===login.toLowerCase()});
      if(!repos.length){lead.textContent='Signed in as '+login+'. GitHub lists no public repositories owned by this account.';return}
      lead.textContent='Signed in as '+login+'. Tick the repositories you want on the Corpus Hub. Each becomes a corpus of the whole repository, credited to you on your contributor page.';
      var t=el('table',{'class':'mystars addrepos'}),hd=el('tr'),boxes=[];
      [['add'],['repository'],['language','opt'],['stars','opt'],['status']].forEach(function(x){hd.appendChild(el('th',x[1]?{'class':x[1]}:{},x[0]))});t.appendChild(hd);
      repos.forEach(function(r){
        var k=r.full_name.toLowerCase(),q=req[k],tr=el('tr'),c=el('td'),why=null;
        if(hub[k])why='on the hub';
        else if(q&&!q.withdrawn)why='added '+(q.at||'').slice(0,10)+'; goes live after the checks';
        else if(r.fork)why='fork: add the original instead';
        else if(r.archived)why='archived';
        else if(!r.size)why='empty';
        if(!why){ // the whole row toggles the box, not only the 22px square
          var cb=el('input',{type:'checkbox',value:r.full_name,'aria-label':'add '+r.full_name});c.appendChild(cb);boxes.push(cb);tr.className='pick';
          tr.onclick=function(e){if(e.target===cb||e.target.closest('a,button')||cb.disabled)return;cb.checked=!cb.checked;upd()}}
        tr.appendChild(c);
        var n=el('td');n.appendChild(el('a',{href:r.html_url,rel:'noopener'},r.full_name));
        if(r.description)n.appendChild(el('div',{'class':'why'},r.description.slice(0,140)));tr.appendChild(n);
        tr.appendChild(el('td',{'class':'mono opt'},r.language||''));
        tr.appendChild(el('td',{'class':'mono opt'},'\u2605 '+Number(r.stargazers_count||0).toLocaleString('en-US')));
        var s=el('td',{'class':'why'},why||'');
        if(hub[k])s.appendChild(document.createTextNode(' \u00b7 ')),s.appendChild(el('a',{href:'/contributors/'+login},'your page'));
        if(q&&!q.withdrawn||hub[k]){var w=el('button',{type:'button','class':'copy'},'withdraw');w.onclick=function(){
          w.disabled=true;api({withdraw:[r.full_name]}).then(function(x){s.textContent=x&&x.ok?'withdrawn: it will be removed from the hub':(x&&x.error)||'could not withdraw';},function(){s.textContent='could not withdraw';w.disabled=false})};
          s.appendChild(document.createTextNode(' '));s.appendChild(w)}
        tr.appendChild(s);t.appendChild(tr);
      });
      var bar=el('div',{'class':'addbar'}),all=el('button',{type:'button','class':'copy'},'select all'),none=el('button',{type:'button','class':'copy'},'select none');
      all.onclick=function(){boxes.forEach(function(b){b.checked=true});upd()};none.onclick=function(){boxes.forEach(function(b){b.checked=false});upd()};
      bar.appendChild(all);bar.appendChild(none);
      var ag=el('label',{'class':'addagree'}),agc=el('input',{type:'checkbox','aria-label':'I agree'});ag.appendChild(agc);ag.appendChild(el('span',{},st.consent.text));
      // the controls sit above the list (it can run to hundreds of rows), with a second Add button under it;
      // the buttons stay clickable and say what is missing instead of greying out
      var gos=[],msg=el('p',{'class':'note',role:'status'}),busy=false;
      function goRow(withMsg){var d=el('div',{'class':'addgo'}),g=el('button',{type:'button','class':'cta'},'Add repositories');g.onclick=add;gos.push(g);d.appendChild(g);if(withMsg)d.appendChild(msg);return d}
      function sel(){return boxes.filter(function(b){return b.checked&&!b.disabled}).map(function(b){return b.value})}
      function upd(){var n=sel().length;boxes.forEach(function(b){b.parentNode.parentNode.classList.toggle('on',b.checked)});
        gos.forEach(function(g){g.textContent=n?'Add '+n+' repositor'+(n===1?'y':'ies'):'Add repositories';g.disabled=busy});
        if(!busy&&n&&agc.checked)msg.textContent=''}
      function say(x){msg.textContent=x;var r=msg.getBoundingClientRect();if(r.top<0||r.bottom>window.innerHeight)msg.scrollIntoView({block:'center'})}
      function add(){
        var list=sel(),done=0;if(busy)return;
        if(!list.length){say(boxes.some(function(b){return !b.disabled})?'Tick at least one repository in the list first.':'Every repository listed is already added or cannot be added.');return}
        if(!agc.checked){say('Tick the agreement box above the button first.');agc.focus();return}
        busy=true;upd();say('Adding\u2026');
        (function next(){ // the API takes up to 100 per request
          var part=list.slice(done,done+100);if(!part.length){
            busy=false;upd();say('Added '+done+' repositor'+(done===1?'y':'ies')+'. Each goes through the same checks as every hub corpus and usually shows on your contributor page within a few hours.');return}
          api({repos:part,agree:true,consent:st.consent.version}).then(function(x){
            if(!x||!x.ok){busy=false;upd();say((x&&x.error)||'Could not add them right now.');return}
            boxes.forEach(function(b){if(part.indexOf(b.value)>=0){b.checked=false;b.disabled=true;var tr=b.parentNode.parentNode;tr.className='';tr.lastChild.textContent='added; goes live after the checks'}});
            done+=part.length;next();
          },function(){busy=false;upd();say('Could not add them right now.')});
        })();
      }
      boxes.forEach(function(b){b.onchange=upd});agc.onchange=upd;
      box.appendChild(ag);box.appendChild(goRow(true));box.appendChild(bar);box.appendChild(t);box.appendChild(goRow(false));upd();
      box.appendChild(el('p',{'class':'note'},'Only repositories under your own account are listed. For organisation repositories, write to founders@xerj.org.'));
    }
  });
})();
</script>"""


NOT_FOUND_BODY = """<section class="hero"><h1>Not found</h1>
<p>There is no page at this address. Corpus pages live at <code>/corpus/&lt;slug&gt;</code>;
<a href="/">the corpus list</a> and <a href="/llms.txt">llms.txt</a> name every live slug.</p></section>"""


def page(title, desc, body, rel="", path="/", image=None, kind="WebPage", noindex=False,
         og_title=None, og_desc=None, ld_extra=None):
    return PAGE.format(title=esc(title), desc=esc(desc), body=body, rel=rel,
                       fonts=FONTS, stamp=STAMP, ver=STYLE_VER, navjs=NAVCTA_JS, authjs=AUTH_JS,
                       ghstars=GH_STARS_BADGE, starsjs=gh_stars.JS,
                       sver=SEARCH_VER, search=search_index.NAV_FORM.format(rel=rel),
                       seo=seo_block(title, desc, path, rel, kind, image, noindex, og_title, og_desc, ld_extra),
                       home=href(f"{rel}index.html"), people=href(f"{rel}contributors/index.html"),
                       add=href(f"{rel}contribute.html"))


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


def render_index(corpora, g7, graded, impact, prov, people=None):
    live = [c for c in corpora if c["status"] == "live"]
    total_bytes = sum(sum(r.get("bytes") or 0 for r in c["repos"]) for c in live)
    # owner, 2026-10-29: "78.7 GB pinned text" reads small; the file count of the same corpora is the headline (bytes in the tooltip)
    total_files = sum(sum(r.get("files") or 0 for r in c["repos"]) for c in live)
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
        installable = [c for c in live_cs if corpus_stats(c)[1] > 0 and has_pack(c)]
        copy = (f'<button class="copy" data-copy="{esc(category_prompt(k, title, installable))}" '
                f'title="Copy a prompt that tells an agent how to ask the {esc(k)} corpora with one line">Copy the prompt</button>')
        tiles = "".join(tile(c) for c in live_cs)
        groups.append(f'''<div class="tgroup">
<div class="thead"><a class="thword" href="category/{k}">{esc(k)}</a>
<div class="tgright">{copy}<span class="tgmeta"><b>{n_live}</b> live · {esc(title)} · {human_bytes(cat_bytes)}</span></div></div>
<div class="tiles" style="height:{band_h}px">{tiles}</div></div>''')
    treemap = "\n".join(groups)
    cat_chips = "".join(f'<button class="chip" data-cat="{k}">{k}</button>' for k in CATS)
    region_chips = "".join(f'<button class="chip" data-region="{esc(r)}">{esc(r)}</button>' for r in regions)
    # owner request: real numbers instead of "registry rows" / "live corpora".
    # Stars: distinct pinned repos with a recorded count only (never estimated).
    stars, n_star_repos = contributors.pinned_repo_stars(people or {})
    stars_stat = (f'<div class="stat" title="{stars:,} GitHub stars, summed over {n_star_repos:,} distinct pinned repositories '
                  f'that have a recorded star count"><b>{stars:,}</b><span>GitHub stars across pinned repos</span></div>'
                  if n_star_repos else "")
    # owner, 2026-10-29: show the repository count (8,1xx) next to the people count (7,2xx): one person can contribute
    # several repositories. Replaces the "11 domains" tile; the domains are the treemap groups right below.
    n_repos = len({w["repo_url"].rstrip("/").lower() for pp in (people or {}).values() for w in pp["work"].values() if w.get("repo_url")})
    # owner, 2026-10-29: "contributed repositories" next to "contributors" read as the same number twice; show the
    # lines of the consented commits instead (contributors.volume, as on each profile; the repo count is in the tooltip)
    n_lines = sum(contributors.volume(pp)["lines"] for pp in (people or {}).values())
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
{stars_stat}
<div class="stat" title="lines in the commits contributors agreed to share, summed over every contributor (a commit counts once), across {n_repos:,} repositories"><a href="contributors/"><b>{n_lines:,}</b><span>lines of contributed code</span></a></div>
<div class="stat"><a href="contributors/"><b>{len(people or {}):,}</b><span>contributors</span></a></div>
<div class="stat" title="{human_bytes(total_bytes)} of text in these files, each pinned at an exact commit"><b>{total_files:,}</b><span>pinned files</span></div>
</div>
<a class="cta" href="contribute">Add your corpus &rarr;</a>
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
{usage.home_section()}
</main>
{FILTER_JS}
{COPY_JS}{usage.USAGE_JS}'''
    return page("XERJ Corpus Hub: reference memory for AI agents",
                "Pinned, licence-checked, measured corpora an agent downloads per task and searches instead of guessing.",
                body, path="/", kind="CollectionPage")


LASTMOD = {}            # out-relative html path -> YYYY-MM-DD, real dates only (write_sitemap)
_TITLES = set()         # registry titles already used
_FACTS = {}             # slug -> GitHub facts line for a single-repo registry corpus


def _date(x):
    x = (x or "")[:10]
    return x if re.match(r"^\d{4}-\d{2}-\d{2}$", x) else ""


def corpus_seo(c, gh, fetched, people, logins):
    """Title, description, JSON-LD, breadcrumbs and lastmod for a registry corpus page."""
    slug = c["slug"]
    single = len(c["repos"]) == 1
    r = c["repos"][0] if single else {}
    st = gh.get(repo_pages.key(r.get("url"))) if single else None
    st = st or {}
    k = repo_pages.key(r.get("url"))
    name = st.get("repo") or repo_pages.repo_name(r.get("url")) or slug
    use = repo_pages.usecase_text(c["usecase"])
    ghd = repo_pages.gh_text(st)
    n = len(c["repos"])
    count = f"{n:,} pinned GitHub repositories"
    if n > 50:                          # the aggregates: the count says more than the blurb
        title = repo_pages.title(slug, count)
    else:
        cat0 = plain(CATS.get(c["cat"], ("", ""))[0])
        fb = (repo_pages.fallback_titles(st.get("language", ""), bool(st)) if single else
              ([f"{{name}}: {cat0} corpus for AI agents"] if cat0 else []) + ["{name}: corpus for AI agents"])
        title = repo_pages.title(name, ghd, use, fallbacks=fb)
        if ":" not in title and n > 1:
            title = repo_pages.title(name, count)
    if title in _TITLES:                # two corpora pin the same repo: name the second by its slug
        title = repo_pages.title(slug, ghd, use, count, fallbacks=repo_pages.fallback_titles(st.get("language", "")))
    _TITLES.add(title)
    head = f"{slug}: {ghd or use}" if (ghd or use) else f"{slug} corpus"
    if ghd and use and len(ghd) < 60:
        head += f". {use}"
    spdx = (r.get("review") or {}).get("spdx") or r.get("licence") or st.get("licence") or ""
    facts = [repo_pages.stars_txt(st.get("stars")), st.get("language", ""),
             spdx if spdx and spdx not in ("?", "NOASSERTION") else ""]
    if c.get("merged_into"):
        tails = [f"Withdrawn: use {c['merged_into']}."]
    elif has_pack(c):
        tails = ["A live XERJ corpus. Copy the prompt to search it with AI.", "Copy the prompt to search it with AI."]
    else:
        lbl = STATUSES.get(c["status"], (c["status"], ""))[0]
        tails = (["Live on XERJ Hub."] if c["status"] == "live" else
                 [f"Status on XERJ Hub: {plain(lbl)}.", f"Status: {plain(lbl)}."])
    desc = repo_pages.description(head, facts, tails)
    cat = CATS.get(c["cat"], ("", ""))[0]
    crumbs = [("Hub", SITE + "/")] + ([(cat, f"{SITE}/category/{c['cat']}")] if cat else []) + [(slug, f"{SITE}/corpus/{slug}")]
    top = repo_pages.crumbs_html([("Hub", "../")] + ([(cat, f"../category/{c['cat']}")] if cat else []) + [(slug, "")], esc)
    pp = people.get(logins[0]) if logins else None
    person = ({"@type": "Person", "name": pp.get("name") or pp["login"], "alternateName": "@" + pp["login"],
               "url": f"{SITE}/contributors/{pp['login']}", "sameAs": [f"https://github.com/{pp['login']}"]} if pp else None)
    if single and k:
        ld = repo_pages.repo_ld(slug, name, r["url"], ghd or use, [st["language"]] if st.get("language") else [], spdx,
                                st.get("topics") or (c.get("tags") or {}).get("topics") or [], person, crumbs, r.get("sha") or "",
                                credit=f"{name}, pinned by the XERJ Corpus Hub")
        ld["mainEntity"]["name"] = f"{slug} corpus"
        ld["mainEntity"]["description"] = repo_pages.clip(
            f"{name} at commit {(r.get('sha') or '')[:10] or 'its pin'}, indexed so AI agents can search it. {use}.", 300)
    else:
        ds = {"@type": "Dataset", "@id": f"{SITE}/corpus/{slug}#dataset", "name": f"{slug} corpus",
              "description": repo_pages.clip(f"{use or slug}. A corpus on the XERJ Corpus Hub that AI agents search.", 300),
              "url": f"{SITE}/corpus/{slug}",
              "includedInDataCatalog": {"@type": "DataCatalog", "name": "XERJ Corpus Hub", "url": SITE + "/"}}
        lic = sorted({repo_pages.spdx_url((x.get("review") or {}).get("spdx") or "") for x in c["repos"]} - {""})
        if lic:
            ds["license"] = lic[0] if len(lic) == 1 else lic
        if c["repos"] and len(c["repos"]) <= 50:
            ds["isBasedOn"] = [x["url"] for x in c["repos"] if x.get("url")]
        kw = (c.get("tags") or {}).get("topics") or []
        if kw:
            ds["keywords"] = kw
        if person:
            ds["contributor"] = person
        ld = {"breadcrumb": repo_pages.crumbs_ld(crumbs), "mainEntity": ds}
    if _date(c.get("added")):
        ld["mainEntity"]["datePublished"] = _date(c.get("added"))
    # the facts line for a single-repo corpus, only what GitHub returned
    bits = []
    if st:
        if isinstance(st.get("stars"), int):
            bits.append(f"{st['stars']:,} GitHub stars")
        if isinstance(st.get("commits"), int):
            bits.append(f"{st['commits']:,} commits on {esc(st.get('branch') or 'the default branch')}")
        if st.get("language"):
            bits.append(esc(st["language"]))
        if st.get("pushed"):
            bits.append(f"last push {esc(st['pushed'])}")
        if isinstance(st.get("pin_behind"), int) and st["pin_behind"] > 0:
            bits.append(f"{st['pin_behind']:,} commits since the pin")
    _FACTS[slug] = ((f'<p class="lede">{esc(ghd)}</p>' if ghd else "")
                    + (f'<p class="note rpfacts">{" · ".join(bits)} · <a href="{esc(r["url"])}">View on GitHub</a>'
                       f'{f" · GitHub data fetched {esc(fetched)}" if fetched else ""}</p>' if bits else ""))
    dates = [_date(c.get("added"))] + [_date((x.get("review") or {}).get("at")) for x in c["repos"][:50]]
    if st and fetched:
        dates.append(_date(fetched))
    lm = max((x for x in dates if x), default="")
    if lm:
        # newest real date we hold for this corpus: added, a licence review, or the GitHub fetch
        ld["mainEntity"]["dateModified"] = lm
        ld["dateModified"] = lm
    return title, desc, ld, top, lm


def facts_html(c):
    return _FACTS.get(c["slug"], "")


def detail_body(c, g7, graded, impact, prov, contrib="", top=""):
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
        label = search_index.github_owner_repo(r.get("url", ""), r.get("repo", "")) or r.get("repo", "")
        rows.append(f'''<tr><td class="mono"><a href="{esc(r.get('url',''))}">{esc(label)}</a></td>
<td class="mono">{esc(r.get('sha','')[:12])}</td><td>{esc(rv.get('spdx','?'))}</td>
<td class="mono">{human_bytes(r.get('bytes'))} / {human_files(r.get('files'))}</td>
<td class="note">{esc(plain(rv.get('note',''))[:400])}</td></tr>''')
    for s in c.get("sources", []):
        # a backlog row's "url" can be a note ("superseded by ...", "mined from ...");
        # link only real URLs, print the rest as text (no dead relative hrefs)
        u, label = s.get('url', ''), esc(s.get('slug', s.get('url', '')))
        src = f'<a href="{esc(u)}">{label}</a>' if re.match(r"^https?://", u or "") else label
        rows.append(f'''<tr><td class="mono">{src}</td>
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
                 f'<a href="{esc(c["merged_into"])}"><code>{esc(c["merged_into"])}</code></a>. '
                 f'Use that corpus.</div>')
    elif has_pack(c):
        s = esc(c["slug"])
        copybtn = (f' <button class="copy" data-copy="{esc(install_prompt(c))}" '
                   f'title="Copy a prompt that asks this corpus with one line">Copy the prompt</button>')
        useit = (f'<div class="kbox"><b>Use it</b>{copybtn}<br>'
                 f'<code>xerj hub pull {s}</code><br>'
                 f'<code>xerj "your question" --corpus {s}</code><br>'
                 f'<code>xerj ask --def SYMBOL --corpus {s}</code>'
                 f'<p class="note">One line, no clone and no index: <code>xerj hub pull {s}</code> '
                 f'fetches this corpus&rsquo;s signed, pre-indexed pack and checks its SHA-256 and '
                 f'signature before restoring it. A question fetches it too the first time, unless '
                 f'the pack is over the automatic size cap. The answer names the '
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
    up = upstream_label(c)
    upline = f'<p class="crepo">{esc(up)}</p>\n' if up else ""
    return f'''<main>
{top or '<p><a class="backlink" href="../">&larr; all corpora</a></p>'}
<h1 class="slug">{esc(c['slug'])} <span class="dot {stcls}"></span></h1>
{upline}{contributors.star_widget("corpus", c["slug"], esc, inline=True)}
<p class="note">{esc(c['kind'])} · status <b>{st}</b> · domain <b>{esc(c['cat'])}</b> ({esc(cat_desc[0])}) · lane {esc(c['lane'])} · refresh {esc(c['refresh'])} · tags <b>{esc(tags_txt)}</b></p>
{facts_html(c)}
<section class="detail"><h2>What an agent uses it for</h2><p>{esc(c['usecase'] or 'n/a')}</p></section>
{useit}
<section class="detail"><h2>Sources and pins</h2><table>
<tr><th>source</th><th>pin</th><th>licence</th><th>size</th><th>review note</th></tr>{''.join(rows)}</table>
<p class="note">{human_bytes(nbytes)} across {human_files(files)}. Added {esc(c['added'][:10] or 'n/a')}.</p></section>
{contrib}
<section class="detail"><h2>Rights</h2><p>{esc(' + '.join(licences) or '?')}.
{esc('Redistribution permitted with attribution per the licence review' if any(r.get('review',{}).get('use')=='adapt-with-attribution' for r in c['repos']) else 'See per-source review blocks')}.
The review block records a human opening each licence file at the pin. Detector output is a hint, never the verdict.</p></section>
{contributors.more_block("corpus", c["slug"], esc)}
{g7sec}{impsec}{agent_score.section(c['slug'], esc)}{usage.traffic_section(c["slug"], esc)}{usage.corpus_section(c["slug"], esc)}
</main>
{COPY_JS}{contributors.STARS_JS}{contributors.MORE_JS}{usage.USAGE_JS}{agent_score.PANEL_JS}'''


def contributed_in(k, people):
    """Author-contributed corpora on shelf k, one row per corpus: (slug, repo, login, stars, spdx), most stars first.
    Owner, 2026-10-29: a contributor found their corpus page live but not on its category page ("got a licence
    clearance but not in live repo"); the category pages listed only the registry corpora."""
    rows = {}
    for login, pp in (people or {}).items():
        for w in pp["work"].values():
            if (w.get("shelf") or "") != k: continue
            for slug in w.get("corpora") or []:
                if slug not in rows: rows[slug] = (slug, w.get("repo") or slug, login, w.get("repo_stars") or 0, w.get("spdx") or "")
    return sorted(rows.values(), key=lambda r: (-r[3], r[1].lower()))


def contributed_section(k, rows):
    if not rows:
        return ""
    trs = "".join(f'<tr><td><a href="../corpus/{esc(s)}">{esc(repo)}</a></td><td><a href="../contributors/{esc(lg)}">@{esc(lg)}</a></td>'
                  f'<td class="mono">{stars:,}</td><td class="mono">{esc(spdx)}</td></tr>' for s, repo, lg, stars, spdx in rows)
    return f'''<section class="detail contributed" id="contributed"><h2>Contributed corpora · {len(rows):,}</h2>
<p class="note">Repositories their authors agreed to share, each pinned at a commit with its licence checked. Every one is live:
its page has the prompt that lets an agent search it now.</p>
<p class="row"><input class="cfilter" type="search" placeholder="filter by repository or contributor" aria-label="filter contributed corpora"></p>
<table class="mystars"><tr><th>repository</th><th>contributor</th><th>GitHub ★</th><th>licence</th></tr>{trs}</table></section>
<script>(function(){{var i=document.querySelector('.cfilter'),rs=[].slice.call(document.querySelectorAll('.contributed tr')).slice(1);
i.addEventListener('input',function(){{var q=i.value.toLowerCase();rs.forEach(function(r){{r.hidden=q&&r.textContent.toLowerCase().indexOf(q)<0}})}})}})();</script>'''


def category_body(k, cs, g7, graded, impact, contributed=()):
    title, what = CATS[k]
    cards = "\n".join(card(c, g7, graded, impact, rel="../") for c in cs)
    n_live = sum(1 for c in cs if c["status"] == "live")
    more = f' <a href="#contributed">{len(contributed):,} contributed corpora</a>.' if contributed else ""
    return f'''<main>
<p><a class="backlink" href="../">&larr; all corpora</a></p>
<div class="kicker"><span class="accent">{esc(k)}</span><span class="dash">·</span><span>DOMAIN</span></div>
<h1 class="hero" style="font-size:clamp(56px,9vw,var(--fs-160))">{esc(k)}</h1>
<p class="lead">{esc(title)}. Agent asks: {esc(what)}. {n_live} live of {len(cs)} entries.{more}</p>
{agent_score.SORT_BAR}
<div class="grid">{cards}</div>
{contributed_section(k, contributed)}
</main>{agent_score.CARDS_JS}'''


# "What's this?" (was About; /about redirects here, see REDIRECTS). Short and
# plain on purpose: what the hub is for, in facts the repo can back up, and
# no numbers that are not computed from the registry at build time.
WHATS_THIS = '''<main class="whats">
<div class="kicker"><span class="accent">WHAT'S THIS?</span></div>
<h1 class="hero" style="font-size:clamp(44px,6vw,var(--fs-96))">A SHARED LIBRARY<br><span class="accent">FOR AI AGENTS</span></h1>
<p class="lead">AI agents often answer from memory, or crawl the web, or grep a repository they cloned a minute ago.
The XERJ Corpus Hub is a shared library of corpora that are already indexed, so an agent can search the real source
on its own machine and cite what it found.</p>
<section class="detail"><h2>How it works</h2><ol class="steps">
<li><b>A corpus is a pinned source.</b> A library's code at one commit, a standard, a regulation, an advisory feed.
Each one records where it came from and the licence a person reviewed.</li>
<li><b>It is pre-indexed and signed.</b> The hub builds a search pack for each corpus. The catalogue and the packs are
signed (ed25519), and the <code>xerj</code> CLI refuses anything that does not verify against the hub key built into it.</li>
<li><b>The agent searches locally.</b> <code>xerj hub pull</code> fetches a pack, then <code>xerj code &lt;corpus&gt; "question"</code>
answers with passages headed by a file path and line, so every answer can be cited as <code>file:line</code>.
Your queries stay on your machine.</li>
</ol></section>
<section class="detail"><h2>People get the credit</h2>
<p>Corpora come from the community. Every contributor has a <a href="{rel}contributors/">profile</a> that lists
their work and the corpora it is in, and visitors can give hub stars to contributors and corpora. Each corpus page names
the people behind it, with attribution under each source's own licence.</p></section>
<section class="detail"><h2>Agents say what worked</h2>
<p>After they use a corpus, agents can send a structured review: was it useful, was the answer found, is it fresh.
That becomes the corpus's <b>agent score</b>, shown on its page apart from the human stars.</p></section>
<section class="detail"><h2>Start</h2>
<p><a href="{rel}">Browse the corpora</a> · <a href="{rel}llms.txt">llms.txt</a> (the agent's guide) ·
<a href="{rel}contribute">Add your corpus</a> · <a href="https://github.com/xerj-org/xerj/tree/corpus-hub/tools/xerj-code/hub">The registry on GitHub</a></p></section>
''' + usage.PRIVACY + '''
</main>'''

# Cloudflare Pages _headers: the signed-in page is never indexed (the page
# also carries <meta name="robots" content="noindex">).
HEADERS = """/me
  X-Robots-Tag: noindex
/me.html
  X-Robots-Tag: noindex
/search/index.json
  X-Robots-Tag: noindex
"""

# Cloudflare Pages _redirects: the old About URLs keep working; every README badge URL
# (/contributors/<login>/badge.svg) is served from the one shared badge file.
REDIRECTS = """/about /whats-this/ 301
/about/ /whats-this/ 301
/about.html /whats-this/ 301
/favicon.ico /favicon.png 200
/contributors/:login/badge.svg /contributors/badge.svg 200
"""


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
<p>Own a repository on GitHub? <a href="/add">Sign in with GitHub and add it</a>: no agent or pull request needed.</p>
<p class="note" style="margin-bottom:var(--sp-4)">Paste this into your coding agent. It explains itself.</p>
<div class="promptbox"><div class="pbh"><button class="copy" data-copy="{prompt}" title="Copy this prompt">Copy the prompt</button></div>
<pre class="prompt">{prompt}</pre></div>
<p class="note">Full guide, templates and the review checklist:
<a href="https://github.com/xerj-org/xerj/blob/corpus-hub/tools/xerj-code/hub/CONTRIBUTING.md">CONTRIBUTING.md</a>.</p>
</section>
{COPY_JS}
</main>'''


def llms_repos(live):
    """llms-repos.txt: one line per repository pinned by a live corpus."""
    seen, rows = set(), []
    for c in sorted(live, key=lambda c: c["slug"]):
        for r in c.get("repos") or []:
            url = r.get("url") or ""
            key = (url.lower(), c["slug"])
            if not url or key in seen:
                continue
            seen.add(key)
            spdx = (r.get("review") or {}).get("spdx") or r.get("licence") or "?"
            sha = (r.get("sha") or "")[:12] or "unpinned"
            rows.append(f"- {repo_pages.repo_name(url) or url} ({url}) @ {sha} · {spdx} · "
                        f"corpus {c['slug']}: {SITE}/corpus/{c['slug']}")
    head = ["# XERJ Corpus Hub: pinned repositories", "",
            f"> {len(rows)} repository pins across {len(live)} live corpora, generated from the registry.",
            "> Each line: repository, the commit the corpus was built from, its licence (SPDX or ?),",
            "> and the corpus page. Ask a corpus with `xerj ask` (see https://hub.xerj.org/llms.txt).",
            f"> Last updated: {STAMP}.", "", "## Repositories", ""]
    return "\n".join(head + rows) + "\n"


def llms_contributors(people, live_slugs):
    """llms-contributors.txt: one line per signed-up contributor."""
    rows = []
    for login in sorted(people, key=str.lower):
        pp = people[login]
        v = contributors.volume(pp, live_slugs)
        name = plain(pp.get("name") or login)
        live = ", ".join(v["live"][:8]) + (" and more" if len(v["live"]) > 8 else "")
        rows.append(f"- {name} (@{login}): {SITE}/contributors/{login} · {v['commits']} selected "
                    f"commit{'s' if v['commits'] != 1 else ''} · {len(v['live'])} live "
                    + ("corpus" if len(v['live']) == 1 else "corpora") + (f": {live}" if live else ""))
    head = ["# XERJ Corpus Hub: contributors", "",
            f"> {len(rows)} developers who agreed to let AI agents retrieve their work, generated from the registry.",
            "> Each line: name, GitHub login, hub page, selected commits, and the live corpora that carry their work.",
            f"> Last updated: {STAMP}.", "", "## Contributors", ""]
    return "\n".join(head + rows) + "\n"


def main():
    OUT.mkdir(exist_ok=True)
    (OUT / "assets").mkdir(exist_ok=True)
    (OUT / "corpus").mkdir(exist_ok=True)
    (OUT / "category").mkdir(exist_ok=True)
    (OUT / "assets" / "style.css").write_text(STYLE)
    write_brand_assets(OUT)
    corpora = load_registry()
    g7 = parse_waves_g7()
    graded = parse_graded_suites()
    impact, prov = load_impact()
    people = contributors.load(HUB, corpora)
    (OUT / "index.html").write_text(render_index(corpora, g7, graded, impact, prov, people))
    # every corpus page credits its people: signed-up contributors, the upstream
    # owners of its pinned repositories, or the curator (contributors.credits)
    credit, upstream = contributors.credits(corpora, people)
    gh_fetched, gh_stats = repo_pages.load_stats()
    LASTMOD.clear()
    for c in corpora:
        cr = credit[c["slug"]]
        contrib = contributors.corpus_section(cr["contributors"], esc, people, cr["upstream"], cr["curator"], cr["aggregate"])
        t, d, ld, top, lm = corpus_seo(c, gh_stats, gh_fetched, people, cr["contributors"])
        if lm:
            LASTMOD[f"corpus/{c['slug']}.html"] = lm
        (OUT / "corpus" / f"{c['slug']}.html").write_text(
            page(t, d, detail_body(c, g7, graded, impact, prov, contrib, top), rel="../",
                 path=f"/corpus/{c['slug']}", ld_extra=ld))
    for k, (title, what) in CATS.items():
        cs = [c for c in corpora if c["cat"] == k]
        (OUT / "category" / f"{k}.html").write_text(
            page(f"{k}: {title} domain", what, category_body(k, cs, g7, graded, impact, contributed_in(k, people)), rel="../",
                 path=f"/category/{k}", kind="CollectionPage"))
    (OUT / "whats-this").mkdir(exist_ok=True)
    (OUT / "whats-this" / "index.html").write_text(page(
        "What's this? XERJ Corpus Hub", "A shared, signed library of pre-indexed corpora that AI agents search locally, with cited file:line answers",
        WHATS_THIS.replace("{rel}", "../"), rel="../", path="/whats-this/"))
    (OUT / "about.html").unlink(missing_ok=True)   # replaced by whats-this/; _redirects keeps the old URL
    (OUT / "_redirects").write_text(REDIRECTS)
    (OUT / "_headers").write_text(HEADERS)
    (OUT / "me.html").write_text(page("Your hub: XERJ Corpus Hub", "Your hub stars, after you sign in", ME_BODY + ME_JS, rel="", path="/me", noindex=True))
    (OUT / "add.html").write_text(page("Add your repositories: XERJ Corpus Hub", "Sign in with GitHub and add the repositories you own to the Corpus Hub",
                                       ADD_BODY + ADD_JS, rel="", path="/add"))
    # 404.html: Cloudflare Pages serves it, with status 404, for any path that
    # has no file. Without it Pages falls back to index.html with a 200, so a
    # mistyped corpus URL, a link checker or an agent probing /corpus/<slug>
    # took the homepage for a live page (issue #1895). Absolute links (rel="/"):
    # it is served at every depth.
    (OUT / "404.html").write_text(page("Not found: XERJ Corpus Hub", "This page does not exist", NOT_FOUND_BODY, rel="/",
                                      path="/404", noindex=True))
    (OUT / "contribute.html").write_text(page("Contribute: XERJ Corpus Hub", "Share your knowledge: index a corpus and open a pull request", contribute_body(), rel="",
                                              path="/contribute"))
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
        up = upstream_label(c)
        who = f"{up} · " if up else ""
        lines.append(f"- [{c['slug']}](https://hub.xerj.org/corpus/{c['slug']}): {who}{c['cat']} · {tags} · {'+'.join(licences) or '?'} · {human_bytes(nbytes)} · {g7s}{ab}; {plain(c['usecase'])[:160]}")
    lines += ["", "## Also in the registry", "",
              f"- {len(corpora) - len(live)} rows in candidate, planned, deferred, killed, or",
              "  withdrawn states (visible, not hidden: hub policy is honest statuses).",
              "  Browse all: https://hub.xerj.org",
              "", "## Search", "",
              "- Every corpus, repository and contributor, as two static JSON files:",
              "  https://hub.xerj.org/search/index.json (names; field list inside) and",
              "  https://hub.xerj.org/search/more.json (languages, topics, use case; same row order)."]
    lines += ["", "## More indexes", "",
              "- https://hub.xerj.org/llms-repos.txt: every repository a live corpus pins, with commit and licence.",
              "- https://hub.xerj.org/llms-contributors.txt: every contributor with a hub page."]
    (OUT / "llms.txt").write_text("\n".join(lines) + "\n")
    (OUT / "llms-repos.txt").write_text(llms_repos(live))
    (OUT / "llms-contributors.txt").write_text(llms_contributors(people, {c["slug"] for c in live}))
    (OUT / f"{INDEXNOW_KEY}.txt").write_text(INDEXNOW_KEY)
    n_people = contributors.generate(OUT, HUB, page, esc, {c["slug"] for c in live}, corpora, people, upstream, cats=CATS,
                                     lastmod=LASTMOD)
    # search: static index + script (search_index.py); no Function, no Worker request
    live_slugs = {c["slug"] for c in live}
    cands = contributors.candidates(people, {c["slug"] for c in corpora}, HUB)
    vols = {login: contributors.volume(pp, live_slugs) for login, pp in people.items()}
    n_sc, n_sp = search_index.write(OUT, search_index.build(corpora, people, cands, upstream, vols, plain, STAMP))
    (OUT / "assets" / "search.js").write_text(search_index.JS)
    (OUT / "search" / "index.html").write_text(page("Search: XERJ Corpus Hub", "Search corpora, repositories and contributors",
                                                    search_index.PAGE_BODY, rel="../", path="/search/", noindex=True))
    reg_slugs = {c["slug"] for c in corpora}
    n_urls = write_sitemap(OUT, LASTMOD, {f"corpus/{s}.html" for s in cands if s not in reg_slugs})
    (OUT / "robots.txt").write_text(ROBOTS)
    # Signed-in details: bundled into the Pages Functions, never written to out/.
    if os.environ.get("GITHUB_ACTIONS") == "true" or os.environ.get("HUB_FUNCTION_DATA") == "1":
        mod = private_data.module(private_data.build(ROOT, HUB, PACKS, corpora, people,
                                                         dt.datetime.now(dt.timezone.utc).strftime("%Y-%m-%d")))
        (ROOT / "functions" / "_lib" / "hubdata.js").write_text(mod)
        print(f"signed-in details: functions/_lib/hubdata.js, {len(mod):,} bytes")
    n = len(list((OUT / "corpus").glob("*.html")))
    print(f"generated: index, whats-this, {len(CATS)} category pages, {n} corpus pages ({len(corpora)} registry), "
          f"{n_people} contributor pages, {len(upstream)} upstream/curator pages, "
          f"sitemap {n_urls} urls -> {OUT}")


if __name__ == "__main__":
    main()
