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
# Mirrors xerj_common::xccode::pathgate::valid_sparse_path (#1842). A paths
# entry becomes the non-cone sparse-checkout pattern /{path}/, so glob and
# negation characters are syntax, not a directory name.
MAX_SPARSE_PATHS = 32
_SPARSE_SPECIAL = set("*?[]!#:")

errors: list[str] = []
# Manifests whose pin is withdrawn are not clone recipes. `read_hub_manifest`
# refuses them before it reads `repos`, so an empty list is the honest shape
# (no url left to clone). They stay in the registry so `--from` can say why.
withdrawn_names: set[str] = set()


def err(msg: str) -> None:
    errors.append(msg)


def sparse_path_error(raw: object) -> str | None:
    """Why `raw` is not a relative directory, or None when it is one."""
    if not isinstance(raw, str):
        return "must be a string"
    if any(ord(c) < 32 or ord(c) == 127 for c in raw):
        return "contains a control character"
    trimmed = raw.strip()
    if not trimmed:
        return "is empty"
    if trimmed.startswith("/") or trimmed.startswith("\\"):
        return "is absolute"
    if (len(trimmed) >= 2 and trimmed[1] == ":"
            and trimmed[0].isascii() and trimmed[0].isalpha()):
        return "is absolute"
    if "\\" in trimmed:
        return "contains a backslash"
    body = trimmed.rstrip("/")
    if not body:
        return "is empty"
    if len(body.encode()) > 240:
        return "is longer than 240 bytes"
    for component in body.split("/"):
        if component in ("", ".", ".."):
            return "escapes the repository"
        if component.startswith("-"):
            return "must not start a path component with '-'"
        if any(c in _SPARSE_SPECIAL for c in component):
            return "contains a glob or gitignore pattern character"
        if any(c.isspace() for c in component):
            return "contains whitespace"
    return None


def check_paths_field(paths: object) -> list[str]:
    """Errors for one repo's `paths` value. The caller skips a missing field."""
    if not isinstance(paths, list):
        return [
            "'paths' must be a non-empty array of relative directories such as "
            '[\"data/osv\"]; omit the field to harvest the whole pin'
        ]
    if not paths:
        return [
            "'paths' is empty; omit the field to harvest the whole pin, "
            "or name at least one directory"
        ]
    if len(paths) > MAX_SPARSE_PATHS:
        return [
            f"'paths' has {len(paths)} entries; at most {MAX_SPARSE_PATHS} "
            "directories can be sparse-checked out. Name a parent directory instead."
        ]
    found: list[str] = []
    for entry in paths:
        reason = sparse_path_error(entry)
        if reason:
            found.append(f"'paths' entry {entry!r} {reason}")
    return found


def _self_check_paths() -> None:
    """The path rules are part of this gate. A drift here fails the run."""
    assert sparse_path_error("data/osv") is None
    assert sparse_path_error("  data/osv/ ") is None
    assert sparse_path_error("docs") is None
    for bad in (
        "", " ", "/", "/etc/passwd", "C:/Windows", "..", "../secret",
        "data/../../etc", "data//osv", "data/./osv", "data/osv/*", "data:osv",
        "!", "#only", "-rf", "data/osv extra", "data/osv\ncmd",
    ):
        assert sparse_path_error(bad) is not None, bad
    assert check_paths_field("data/osv")
    assert check_paths_field([])
    assert check_paths_field(["data/osv", "cmd"]) == []
    assert check_paths_field(["../secret"])
    assert check_paths_field([f"d{i}" for i in range(MAX_SPARSE_PATHS + 1)])


_self_check_paths()


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
        # Optional. Absent harvests the whole pin. Present is the subpath
        # `corpus add --from` sparse-checks out (#1842).
        if "paths" in r:
            for msg in check_paths_field(r.get("paths")):
                err(f"{where}: {msg}")
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

# ── Author-approved candidates ────────────────────────────────────────────
# candidates/authors/ is not a live registry (review.use may be UNREVIEWED
# until G4), but a push still has to be internally consistent: one manifest
# per slug, a GitHub pin, a description, attribution, a star count, and a
# ledger row whose repo URL matches. Two repo URLs under one slug double-count
# stars (bherbruck/solidcraft and tangency-labs/solidcraft were one rename).
AUTHORS = HUB / "candidates" / "authors"
CANDIDATE_USE = LEGAL_USE | {"UNREVIEWED"}
CANDIDATE_AT = re.compile(r"^\d{4}-\d{2}-\d{2}(?:T\d{2}:\d{2}(?::\d{2})?Z)?$")
CANDIDATE_LOGIN = re.compile(r"^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})$")
GH_REPO = re.compile(r"^https://github\.com/([^/\s]+)/([^/\s#?]+?)(?:\.git)?/?$", re.I)
LEDGER_MAX = 5 * 1024 * 1024


def _repo_url(url: object) -> str:
    return str(url or "").strip().rstrip("/").removesuffix(".git").lower()


# Standing opt-outs (candidates/do-not-contact.json, issue-tracked). A refresh
# that re-adds a no-include login or repository fails validation, so an owner
# opt-out recorded in an issue cannot be silently reverted by a regeneration.
DNC_PATH = HUB / "candidates" / "do-not-contact.json"
DNC_BANNED: dict[str, int] = {}  # login-as-owner or owner/repo -> issue
if DNC_PATH.exists():
    for e in json.loads(DNC_PATH.read_text(encoding="utf-8")).get("entries", []):
        if "no-include" not in (e.get("actions") or []):
            continue
        issue = e.get("issue")
        for key in ("login", "also_login"):
            if e.get(key):
                DNC_BANNED[str(e[key]).lower()] = issue
        if e.get("repo"):
            DNC_BANNED[_repo_url(e["repo"])] = issue


def _dnc_hit(login: object, repo_url: object) -> int | None:
    repo = _repo_url(repo_url)
    owner = repo.split("/")[0] if "/" in repo else ""
    for key in (str(login or "").lower(), repo, owner):
        if key in DNC_BANNED:
            return DNC_BANNED[key]
    return None


author_files = sorted(AUTHORS.glob("*.json")) if AUTHORS.is_dir() else []
ledger_rows: dict[str, list[dict]] = {}
if AUTHORS.is_dir():
    ledgers = sorted(AUTHORS.glob("CONTRIBUTORS*.jsonl"))
    if not ledgers:
        err(f"{AUTHORS.relative_to(ROOT)}: no CONTRIBUTORS*.jsonl ledger")
    for ledger in ledgers:
        if ledger.stat().st_size > LEDGER_MAX:
            err(f"{ledger.relative_to(ROOT)}: {ledger.stat().st_size} bytes exceeds the {LEDGER_MAX}-byte ledger limit")
        for n, line in enumerate(ledger.read_text().splitlines(), 1):
            if not line.strip():
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError as e:
                err(f"{ledger.relative_to(ROOT)}:{n}: not valid JSON ({e})")
                continue
            slug = row.get("corpus")
            if not isinstance(slug, str) or not slug:
                err(f"{ledger.relative_to(ROOT)}:{n}: missing corpus")
                continue
            lic = row.get("licence")
            if isinstance(lic, dict) and "author_grant" in lic:
                err(f"{ledger.relative_to(ROOT)}:{n}: licence.author_grant is not an "
                    f"established field — a consent reply covers inclusion under the "
                    f"repository's licence; exceptional terms need an owner-approvals "
                    f"entry. On a refresh branch, run "
                    f"python3 tools/xerj-code/hub/candidates/strip_author_grants.py (see #2791)")
            ledger_rows.setdefault(slug, []).append(row)

seen_author_slugs: set[str] = set()
for path in author_files:
    rel = path.relative_to(ROOT)
    try:
        data = json.loads(path.read_text())
    except json.JSONDecodeError as e:
        err(f"{rel}: not valid JSON ({e})")
        continue
    slug = data.get("corpus")
    if slug != path.stem:
        err(f"{rel}: corpus field {slug!r} must equal the filename {path.stem!r}")
        continue
    if slug in seen_author_slugs:
        err(f"{rel}: duplicate candidate slug {slug!r}")
    seen_author_slugs.add(slug)
    if not isinstance(data.get("domain"), str) or not data["domain"].strip():
        err(f"{rel}: missing domain (the description an agent would query)")
    repos = data.get("repos")
    if not isinstance(repos, list) or len(repos) != 1 or not isinstance(repos[0], dict):
        err(f"{rel}: repos must be a one-element list")
        continue
    repo = repos[0]
    where = f"{rel}: repos[0]"
    url = repo.get("url")
    if not isinstance(url, str) or not GH_REPO.match(url.strip()):
        err(f"{where}: url must be https://github.com/<owner>/<repo>")
    sha = repo.get("sha") or ""
    if not SHA_RE.match(sha):
        err(f"{where}: sha must be full 40-hex, got {sha!r}")
    if not repo.get("licence"):
        err(f"{where}: missing licence")
    review = repo.get("review")
    if not isinstance(review, dict):
        err(f"{where}: missing review block")
    else:
        use = review.get("use")
        if use not in CANDIDATE_USE:
            err(f"{where}: review.use must be one of {sorted(CANDIDATE_USE)} or UNREVIEWED, got {use!r}")
        if use in LEGAL_USE:
            for field in ("spdx", "by", "at"):
                if not review.get(field):
                    err(f"{where}: review missing {field}")
            if review.get("at") and not CANDIDATE_AT.match(review["at"]):
                err(f"{where}: review.at must be YYYY-MM-DD or an ISO minute, got {review.get('at')!r}")
        if "author_grant" in review:
            err(f"{where}: review.author_grant is not an established field — a consent "
                f"reply covers inclusion under the repository's licence; exceptional "
                f"terms need an owner-approvals entry. On a refresh branch, run "
                f"python3 tools/xerj-code/hub/candidates/strip_author_grants.py (see #2791)")
    rows_for = ledger_rows.get(slug) or []
    if not rows_for:
        err(f"{rel}: no ledger row in CONTRIBUTORS*.jsonl (no attribution, stars, or contributor page)")
        continue
    urls = {_repo_url(r.get("repo_url")) for r in rows_for}
    if len(urls) != 1:
        err(f"{rel}: ledger rows name {sorted(urls)} — one corpus slug must be one repository URL")
    elif _repo_url(url) and _repo_url(url) not in urls:
        err(f"{rel}: manifest url {url!r} does not match the ledger ({next(iter(urls))})")
    for r in rows_for:
        issue = _dnc_hit((r.get("contributor") or {}).get("login"), r.get("repo_url"))
        if issue:
            err(f"{rel}: {r.get('repo_url')} is on candidates/do-not-contact.json "
                f"(issue {issue}) - an opt-out cannot be re-added by a refresh")
    pins = set()
    for r in rows_for:
        pin = (r.get("pin") or {}).get("sha") or (r.get("contribution") or {}).get("commit") or ""
        if pin:
            pins.add(pin.lower())
        who = (r.get("contributor") or {}).get("login") or ""
        if not CANDIDATE_LOGIN.match(who):
            err(f"{rel}: contributor login {who!r} is not a GitHub login")
        if not isinstance(r.get("repo_stars"), int):
            err(f"{rel}: repo_stars must be an integer, got {r.get('repo_stars')!r}")
    if sha and sha.lower() not in pins:
        err(f"{rel}: manifest sha {sha[:12]} is not a ledger pin")

for slug, rows_for in sorted(ledger_rows.items()):
    if slug not in seen_author_slugs and slug not in {p.stem for p in manifests}:
        err(f"candidates/authors ledger: {slug} has no manifest")

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
