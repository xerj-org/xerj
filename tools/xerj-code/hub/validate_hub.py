#!/usr/bin/env python3
"""Registry validation for the corpus-hub branch (§5 of CONTRIBUTING.md).

Checks every tools/xerj-code/hub/*.json manifest and every
tools/packs/*/recipe.toml against the registry's format rules, and the
PROGRAM-100 backlog against its own schema. Deliberately stdlib-only and
engine-free: this is the fast gate a PR runs; the real untrusted-input gate
consumers use is `cargo test -p xerj-common xccode::manifest`, which CI on
main also runs over these files.
"""
from __future__ import annotations

import json
import pathlib
import re
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parents[3]
HUB = ROOT / "tools" / "xerj-code" / "hub"
PACKS = ROOT / "tools" / "packs"
BACKLOG = HUB / "backlog" / "backlog-100.json"

LEGAL_USE = {"adapt-with-attribution", "approach-only", "mixed"}
SHA_RE = re.compile(r"^[0-9a-f]{40}$")

errors: list[str] = []
# Manifests whose pin is withdrawn are not clone recipes. `read_hub_manifest`
# refuses them before it reads `repos`, so an empty list is the honest shape
# (no url left to clone). They stay in the registry so `--from` can say why.
withdrawn_names: set[str] = set()


def err(msg: str) -> None:
    errors.append(msg)


# ── Lane A: reference-corpus manifests ────────────────────────────────────
manifests = sorted(p for p in HUB.glob("*.json") if p.name != "TEMPLATE.json")
if not manifests:
    err("no hub manifests found — did the tree change shape?")

for path in manifests:
    rel = path.relative_to(ROOT)
    try:
        data = json.loads(path.read_text())
    except json.JSONDecodeError as e:
        err(f"{rel}: not valid JSON ({e})")
        continue

    name = data.get("corpus")
    if name != path.stem:
        err(f"{rel}: corpus field {name!r} must equal the filename {path.stem!r}")

    # A withdrawn pin is refused by the clone path. It must say why, and it
    # must not be held to the clone-recipe rules (a non-empty `repos` list,
    # a sha, a licence review). PROGRAM-100 records that state as backlog
    # status `withdrawn`.
    withdrawn = data.get("withdrawn")
    if withdrawn is not None:
        if not isinstance(withdrawn, str) or not withdrawn.strip():
            err(f"{rel}: withdrawn must be a non-empty string")
        else:
            withdrawn_names.add(path.stem)
        continue

    repos = data.get("repos")
    if not isinstance(repos, list) or not repos:
        err(f"{rel}: repos must be a non-empty list")
        continue
    for i, r in enumerate(repos):
        where = f"{rel}: repos[{i}] ({r.get('repo', '?')})"
        for field in ("repo", "url", "licence", "sha"):
            if not r.get(field):
                err(f"{where}: missing {field}")
        if r.get("sha") and not SHA_RE.match(r["sha"]):
            err(f"{where}: sha must be full 40-hex, got {r['sha']!r}")
        review = r.get("review")
        if not isinstance(review, dict):
            err(f"{where}: missing human licence review block (see CONTRIBUTING.md §3)")
            continue
        for field in ("spdx", "use", "by", "at"):
            if not review.get(field):
                err(f"{where}: review missing {field}")
        if review.get("use") not in LEGAL_USE:
            err(f"{where}: review.use must be one of {sorted(LEGAL_USE)}, "
                f"got {review.get('use')!r}")
        if review.get("at") and not re.match(r"^\d{4}-\d{2}-\d{2}$", review["at"]):
            err(f"{where}: review.at must be YYYY-MM-DD")

# ── Lane B: pack recipes ──────────────────────────────────────────────────
recipes = sorted(p for p in PACKS.glob("*/recipe.toml")
                 if p.parent.name != "TEMPLATE-recipe")
if not recipes:
    err("no pack recipes found — did the tree change shape?")

for path in recipes:
    rel = path.relative_to(ROOT)
    try:
        recipe = tomllib.loads(path.read_text())
    except tomllib.TOMLDecodeError as e:
        err(f"{rel}: not valid TOML ({e})")
        continue

    meta = recipe.get("recipe", {})
    if meta.get("name") != path.parent.name:
        err(f"{rel}: recipe.name {meta.get('name')!r} must equal the directory "
            f"{path.parent.name!r}")
    if meta.get("format") != 1:
        err(f"{rel}: recipe.format must be 1 (unknown formats are refused by "
            "the tool — do not invent one)")
    sources = recipe.get("sources")
    if not isinstance(sources, list) or not sources:
        err(f"{rel}: at least one [[sources]] block is required")
        continue
    for i, s in enumerate(sources):
        where = f"{rel}: sources[{i}]"
        for field in ("slug", "kind", "licence"):
            if not s.get(field):
                err(f"{where}: missing {field}")
        # dir sources carry a recipe-relative path; the engine REFUSES a url
        # on them (harvest/recipe.rs), so requiring one here would reject
        # every valid dir recipe. First such recipe: xerj-blogposts.
        if s.get("kind") == "dir":
            if not s.get("path"):
                err(f"{where}: dir source needs a 'path' (recipe-relative)")
        elif not s.get("url"):
            err(f"{where}: missing url")
        if s.get("kind") not in ("git", "http-zip", "dir"):
            err(f"{where}: kind must be git | http-zip | dir, got {s.get('kind')!r}")
        if "/" in s.get("slug", "") or ".." in s.get("slug", ""):
            err(f"{where}: slug must be a plain name, never a path "
                "(it is joined into filesystem paths)")

if errors:
    print("corpus-hub validation FAILED:")
    for e in errors:
        print(f"  - {e}")
    sys.exit(1)

# ── PROGRAM-100 backlog consistency ───────────────────────────────────────
# The backlog is the work list PROGRAM-100.md runs on; it must agree with the
# registry it drives: every live manifest/recipe has a `live` row, no slug is
# reused, and the enum fields match the program's vocabulary. drafts/ is a
# staging area by design and is never validated here — a draft's UNREVIEWED
# review block must not be able to reach the registry by accident.
backlog_errors: list[str] = []
try:
    backlog = json.loads(BACKLOG.read_text())
except FileNotFoundError:
    backlog_errors.append(f"{BACKLOG.relative_to(ROOT)}: missing")
    backlog = {}
except json.JSONDecodeError as e:
    backlog_errors.append(f"{BACKLOG.relative_to(ROOT)}: not valid JSON ({e})")
    backlog = {}

rows = backlog.get("corpora", [])
slugs: dict[str, str] = {}
for i, row in enumerate(rows):
    where = f"backlog corpora[{i}]"
    for field in ("slug", "cat", "lane", "refresh", "status", "domain", "sources"):
        if not row.get(field):
            backlog_errors.append(f"{where}: missing {field}")
    slug = row.get("slug", "")
    if slug:
        if slug in slugs:
            backlog_errors.append(f"{where}: duplicate slug {slug!r} "
                                  f"(also {slugs[slug]})")
        slugs[slug] = where
    if "/" in slug or ".." in slug:
        backlog_errors.append(f"{where}: slug must be a plain name, never a path")
    if row.get("status") not in backlog.get("statuses", []):
        backlog_errors.append(f"{where}: status {row.get('status')!r} not in "
                              f"backlog.statuses")
    if row.get("cat") not in backlog.get("cats", []):
        backlog_errors.append(f"{where}: cat {row.get('cat')!r} not in backlog.cats")
    if row.get("lane") not in backlog.get("lanes", []):
        backlog_errors.append(f"{where}: lane {row.get('lane')!r} not in backlog.lanes")
    if row.get("refresh") not in backlog.get("refreshes", []):
        backlog_errors.append(f"{where}: refresh {row.get('refresh')!r} not in "
                              f"backlog.refreshes")

live_manifests = {p.stem for p in manifests if p.stem not in withdrawn_names}
for name in sorted(withdrawn_names):
    row = next((r for r in rows if r.get("slug") == name), None)
    if row is None:
        backlog_errors.append(f"withdrawn manifest {name}.json has no backlog row")
    elif row.get("status") != "withdrawn":
        backlog_errors.append(
            f"withdrawn manifest {name}.json has backlog status "
            f"{row.get('status')!r}, not 'withdrawn'"
        )
    elif row.get("lane") != "A":
        backlog_errors.append(
            f"withdrawn manifest {name}.json is lane A by construction; "
            f"backlog row says {row.get('lane')!r}"
        )
live_recipes = {p.parent.name for p in recipes}
for name in sorted(live_manifests):
    row = next((r for r in rows if r.get("slug") == name), None)
    if row is None:
        backlog_errors.append(f"manifest {name}.json has no backlog row")
    elif row.get("status") != "live":
        backlog_errors.append(f"manifest {name}.json exists but backlog status "
                              f"is {row.get('status')!r}, not 'live'")
    elif row.get("lane") != "A":
        backlog_errors.append(f"manifest {name}.json is lane A by construction; "
                              f"backlog row says {row.get('lane')!r}")
for name in sorted(live_recipes):
    if name in live_manifests:
        # Hybrid: a lane-A manifest registers the corpus and references the
        # recipe (rebuild reproducibility); the manifest row covers both.
        continue
    row = next((r for r in rows if r.get("slug") == name), None)
    if row is None:
        backlog_errors.append(f"recipe {name} has no backlog row")
    elif row.get("status") != "live":
        backlog_errors.append(f"recipe {name} exists but backlog status is "
                              f"{row.get('status')!r}, not 'live'")
    elif row.get("lane") != "B":
        backlog_errors.append(f"recipe {name} is lane B by construction; backlog "
                              f"row says {row.get('lane')!r}")

if backlog_errors:
    print("corpus-hub validation FAILED:")
    for e in backlog_errors:
        print(f"  - {e}")
    sys.exit(1)

live_rows = sum(1 for r in rows if r.get("status") == "live")
print(f"corpus-hub validation OK: {len(manifests)} manifests, "
      f"{len(recipes)} recipes, backlog {len(rows)} rows "
      f"({live_rows} live)")
