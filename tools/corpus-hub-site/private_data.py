"""Signed-in extras for hub.xerj.org: the data behind /api/v1/contributors/<login>
and /api/v1/corpus/<slug> (functions/api/v1/...).

This data is NOT written into the public site. gen.py serialises it into
functions/_lib/hubdata.js, which wrangler bundles into the Pages Functions at
deploy time, so it is only reachable through the authenticated API. The
committed hubdata.js is a stub (`null`, meaning "no details in this deploy").
gen.py replaces it only in CI (GITHUB_ACTIONS=true) or with HUB_FUNCTION_DATA=1,
so a local run does not dirty the tree.

Everything here is read from registry files already on this branch:
  * the contributor ledger and themed tables (contributors.load)
  * candidates/authors/<corpus>.json and the lane-A manifests (pins, sizes, review)
  * benchmarks/hub-backfill/rows.jsonl (pack build history, one row per corpus per run)
  * tools/packs/<pack>/pack-stats.json and tools/packs/keys/*.pub (lane B)
  * engine/crates/xerj-common/src/hub.rs HUB_PUBKEY_HEX (the key packs are signed with)

Never included: consent channel/scope or any e-mail, IP-derived data, star
sources. The consent DATE is already public on every contributor page.
"""
import json
import pathlib
import re

ROWS_KEEP = ("run", "date", "pin8", "bytes", "defs", "result", "tag", "stage", "error", "url")
REPO_KEEP = ("repo", "url", "sha", "files", "bytes", "licence")
REVIEW_KEEP = ("spdx", "use", "by", "at", "note")
MAX_BUILDS = 40           # per corpus, newest first
MAX_REPOS = 60            # a corpus with more sources (exploit-pocs-*) gets a summary;
                          # its public page already lists every source and pin


def _json(p):
    try:
        return json.loads(pathlib.Path(p).read_text())
    except (OSError, ValueError):
        return None


def build_rows(root):
    """slug -> pack build rows, newest first (benchmarks/hub-backfill/rows.jsonl)."""
    out = {}
    p = pathlib.Path(root) / "benchmarks" / "hub-backfill" / "rows.jsonl"
    if not p.exists():
        return out
    for line in p.read_text().splitlines():
        try:
            r = json.loads(line)
        except ValueError:
            continue
        if isinstance(r, dict) and r.get("slug"):
            out.setdefault(r["slug"], []).append({k: r.get(k) for k in ROWS_KEEP})
    for slug, rows in out.items():
        rows.sort(key=lambda r: (str(r.get("date") or ""), str(r.get("run") or "")), reverse=True)
        out[slug] = rows[:MAX_BUILDS]
    return out


def pin_history(rows):
    """Distinct pins seen by the pack pipeline, oldest first, with first/last
    build date and the release tag if one was published at that pin."""
    pins = {}
    for r in sorted(rows, key=lambda r: (str(r.get("date") or ""), str(r.get("run") or ""))):
        p8 = r.get("pin8")
        if not p8:
            continue
        e = pins.setdefault(p8, {"pin8": p8, "first": r.get("date"), "last": r.get("date"), "builds": 0, "published": ""})
        e["last"] = r.get("date")
        e["builds"] += 1
        if r.get("result") == "published" and r.get("tag"):
            e["published"] = r["tag"]
    return list(pins.values())


def _repo(r):
    d = {k: r.get(k) for k in REPO_KEEP if r.get(k) not in (None, "")}
    if r.get("review"):
        d["review"] = {k: r["review"].get(k, "") for k in REVIEW_KEEP}
    return d


def hub_pubkey(root):
    p = pathlib.Path(root) / "engine" / "crates" / "xerj-common" / "src" / "hub.rs"
    try:
        m = re.search(r'pub const HUB_PUBKEY_HEX: &str = "([0-9a-fA-F]*)"', p.read_text())
    except OSError:
        return ""
    return m.group(1) if m else ""


def build(root, hub_dir, packs_dir, corpora, people, stamp):
    root, hub, packs = pathlib.Path(root), pathlib.Path(hub_dir), pathlib.Path(packs_dir)
    rows = build_rows(root)
    by_slug = {c["slug"]: c for c in corpora}
    in_corpus = {}
    for login, pp in people.items():
        for slug in pp["corpora"]:
            in_corpus.setdefault(slug, []).append(login)

    def manifest_status(slug):
        m = _json(hub / "candidates" / "authors" / f"{slug}.json") or _json(hub / "candidates" / f"{slug}.json")
        return (m or {}).get("status", "")

    def last_build(slug):
        r = (rows.get(slug) or [None])[0]
        return {k: r[k] for k in ("date", "pin8", "result", "tag", "url") if r and r.get(k)} if r else None

    contribs = {}
    for login, pp in people.items():
        work = []
        for w in sorted(pp["work"].values(), key=lambda w: (str(w.get("committed_at") or w.get("agreed") or "")), reverse=True):
            cs = []
            for slug in w["corpora"]:
                c = by_slug.get(slug)
                cs.append({k: v for k, v in {
                    "slug": slug, "page": bool(c), "status": (c or {}).get("status", ""),
                    "manifest": manifest_status(slug), "last_build": last_build(slug)}.items() if v not in (None, "")})
            work.append({k: v for k, v in {
                "repo": w["repo"], "repo_url": w["repo_url"], "repo_stars": w.get("repo_stars"),
                "commit": w["commit"], "title": w["title"],
                # commit_url is repo_url/commit/<sha> unless the source says otherwise
                "commit_url": "" if w["commit_url"] == f'{w["repo_url"].rstrip("/")}/commit/{w["commit"]}' else w["commit_url"],
                "committed_at": w.get("committed_at", ""), "files": w.get("files"), "lines": w.get("lines"),
                "langs": w.get("langs") or [], "areas": w.get("areas") or [], "topics": w.get("topics") or [],
                "shelf": w.get("shelf", ""), "spdx": w.get("spdx", ""), "suggested_use": w.get("suggested_use", ""),
                "reviewed": bool(w.get("reviewed")), "agreed": (w.get("agreed") or "")[:10],
                "pin": "" if w.get("pin") == w["commit"] else w.get("pin", ""),   # "" = pinned to the commit
                "corpora": cs}.items() if v not in (None, "", [])})
        also = {s: rel for s, rel in pp["corpora"].items() if not any(s in w["corpora"] for w in pp["work"].values())}
        contribs[login.lower()] = {"login": login, "name": pp["name"], "work": work, "also_pinned_in": also}

    corp = {}
    for c in corpora:
        slug = c["slug"]
        entry = {"slug": slug, "lane": c["lane"], "status": c["status"], "refresh": c.get("refresh", ""),
                 "added": c.get("added", "")}
        repos = c.get("repos") or []
        if len(repos) <= MAX_REPOS:
            entry["repos"] = [_repo(r) for r in repos]
        else:
            entry["repos_summary"] = {
                "count": len(repos), "files": sum(r.get("files") or 0 for r in repos),
                "bytes": sum(r.get("bytes") or 0 for r in repos),
                "reviewed": sum(1 for r in repos if (r.get("review") or {}).get("by")),
                "licences": sorted({(r.get("review") or {}).get("spdx") or "?" for r in repos})[:20]}
        if rows.get(slug):
            entry["builds"] = rows[slug]
            entry["pins"] = pin_history(rows[slug])
        stats = _json(packs / slug / "pack-stats.json")
        if isinstance(stats, dict):
            entry["pack_stats"] = stats
        key = packs / "keys" / f"{slug}.pub"
        if key.exists():
            entry["pack_pubkey"] = key.read_text().strip()[:400]
        if in_corpus.get(slug):
            entry["contributors"] = sorted(in_corpus[slug], key=str.lower)
        corp[slug] = entry

    return {"v": 1, "generated": stamp, "hub_pubkey": hub_pubkey(root),
            "contributors": contribs, "corpora": corp}


STUB = """// Signed-in details for hub.xerj.org (/api/v1/contributors, /api/v1/corpus).
// STUB: this deploy carries no details. tools/corpus-hub-site/gen.py replaces this
// file in CI with the generated data before `wrangler pages deploy` bundles it;
// see tools/corpus-hub-site/private_data.py. Do not commit the generated version.
export default null;
"""


def module(data):
    """The generated functions/_lib/hubdata.js: one JSON string, parsed lazily."""
    raw = json.dumps(data, separators=(",", ":"), ensure_ascii=True)
    return ("// GENERATED by tools/corpus-hub-site/gen.py (private_data.py) at deploy time.\n"
            "// Not served as a file: bundled into the Pages Functions. Do not commit.\n"
            f"export default {json.dumps(raw)};\n")
