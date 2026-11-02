#!/usr/bin/env python3
"""Owner overrides for the Corpus Hub author candidates.

The outreach pipeline (`hub_publish.py`, run outside this repository) rewrites
everything under `authors/` on each refresh from its own consent records. When
a repository owner asks for a change (another featured commit, a re-pin, the
whole repository instead of one commit, a renamed repository, credit moved to
another repo), the maintainers approve it in an issue and record it in
`owner-overrides.json` next to this script. This script re-applies those
records on top of whatever the pipeline generated, so an approved owner
request survives every refresh:

    python3 tools/xerj-code/hub/candidates/owner_overrides.py          # apply
    python3 tools/xerj-code/hub/candidates/owner_overrides.py --check  # CI

`--check` changes nothing and exits 1 when any manifest, ledger row
(CONTRIBUTORS*.jsonl) or CONTRIBUTORS.md row disagrees with an override, so a
refresh pull request that reverts an owner request fails `corpus-hub validate`
instead of being merged. Run the apply mode on the refresh branch to fix it.

Override record (one per corpus slug, see owner-overrides.json):

    {
      "corpus": "mindemory--proxenos",       # slug after the change
      "issue": 2831,                          # where the owner asked
      "replaces": "old--slug",                # optional: slug the pipeline
                                              #   still emits (renames/moves)
      "manifest": {"repos.0.sha": "...", ...},# dotted path -> value
      "ledger":   {"pin.sha": "...", ...}     # dotted path -> value
    }

Values replace what is there (no merging); `null` sets null. The
CONTRIBUTORS.md row of an overridden entry is re-rendered from its ledger row
in the pipeline's own format. Stdlib only.
"""
from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
OVERRIDES = HERE / "owner-overrides.json"
AUTHORS = HERE / "authors"


def load_overrides(path: Path = OVERRIDES) -> list[dict]:
    data = json.loads(path.read_text(encoding="utf-8"))
    entries = data["overrides"]
    seen = set()
    for e in entries:
        for key in ("corpus", "issue", "manifest", "ledger"):
            if key not in e:
                raise ValueError(f"override {e.get('corpus')!r}: missing {key!r}")
        if e["corpus"] in seen:
            raise ValueError(f"override {e['corpus']!r} listed twice")
        seen.add(e["corpus"])
    return entries


def set_path(obj, dotted: str, value) -> None:
    parts = dotted.split(".")
    for p in parts[:-1]:
        obj = obj[int(p)] if isinstance(obj, list) else obj.setdefault(p, {})
    last = parts[-1]
    if isinstance(obj, list):
        obj[int(last)] = value
    else:
        obj[last] = value


def patch(doc: dict, fields: dict) -> dict:
    out = json.loads(json.dumps(doc))
    for k, v in fields.items():
        set_path(out, k, v)
    return out


def dump_manifest(doc: dict) -> str:
    return json.dumps(doc, indent=1, ensure_ascii=True) + "\n"


def dump_row(row: dict) -> str:
    return json.dumps(row, ensure_ascii=False)


def render_md_row(r: dict) -> str:
    """One CONTRIBUTORS.md table row, in the pipeline's format."""
    c = r["contributor"]
    who = f"[@{c['login']}]({c['profile']})"
    if c.get("name"):
        who += " " + c["name"].replace("|", "/")
    if c.get("owns_repo"):
        who += " (owner)"
    k = r.get("contribution") or {}
    if k.get("commit"):
        title = (k.get("title") or "")[:60].replace("|", "\\|")
        contrib = f"[{k['commit'][:10]}]({k['commit_url']}) {title}"
    else:
        contrib = "repo head"
    return (f"| [{r['repo']}]({r['repo_url']}) | {who} | {r['consent']['at']} "
            f"| {contrib} | {r['licence']['spdx']} | {r['repo_stars']} |")


def _ledger_files(authors: Path) -> list[Path]:
    return sorted(authors.glob("CONTRIBUTORS*.jsonl"))


def plan(authors: Path, overrides: list[dict]) -> tuple[dict, list[Path], list[str]]:
    """Compute desired file contents. Returns (writes, deletes, problems)."""
    writes: dict[Path, str] = {}
    deletes: list[Path] = []
    problems: list[str] = []

    ledgers = {p: p.read_text(encoding="utf-8").splitlines() for p in _ledger_files(authors)}
    md_path = authors / "CONTRIBUTORS.md"
    md = md_path.read_text(encoding="utf-8").splitlines() if md_path.exists() else []
    md_changed = False

    for e in overrides:
        slug, old = e["corpus"], e.get("replaces")
        names = [slug] + ([old] if old else [])

        # manifest
        src = next((authors / f"{n}.json" for n in names if (authors / f"{n}.json").exists()), None)
        if src is None:
            problems.append(f"{slug}: no manifest ({' or '.join(names)}.json); "
                            "the pipeline dropped it (opt-out?) - not re-adding")
            continue
        want = dump_manifest(patch(json.loads(src.read_text(encoding="utf-8")),
                                   {"corpus": slug, **e["manifest"]}))
        dst = authors / f"{slug}.json"
        if not dst.exists() or dst.read_text(encoding="utf-8") != want:
            writes[dst] = want
        if old and (authors / f"{old}.json").exists():
            deletes.append(authors / f"{old}.json")

        # ledger row (exactly one across the CONTRIBUTORS*.jsonl shards)
        hits = [(p, i) for p, lines in ledgers.items() for i, l in enumerate(lines)
                if l.strip() and json.loads(l).get("corpus") in names]
        if not hits:
            problems.append(f"{slug}: no ledger row")
            continue
        (lp, li), dupes = hits[0], hits[1:]
        before = json.loads(ledgers[lp][li])
        row = patch(before, {"corpus": slug, **e["ledger"]})
        ledgers[lp][li] = dump_row(row)
        for dp, di in sorted(dupes, key=lambda x: -x[1]):
            del ledgers[dp][di]

        # CONTRIBUTORS.md row, matched by the generated row's repo link
        old_link = f"| [{before['repo']}]({before['repo_url']}) |"
        new_link = f"| [{row['repo']}]({row['repo_url']}) |"
        idx = [i for i, l in enumerate(md) if l.startswith(old_link) or l.startswith(new_link)]
        if md and not idx:
            problems.append(f"{slug}: no CONTRIBUTORS.md row")
        for i in idx:
            r = render_md_row(row)
            if md[i] != r:
                md[i] = r
                md_changed = True

    for p, lines in ledgers.items():
        text = "\n".join(lines) + "\n" if lines else ""
        if text != p.read_text(encoding="utf-8"):
            writes[p] = text
    if md_changed:
        writes[md_path] = "\n".join(md) + "\n"
    return writes, deletes, problems


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--check", action="store_true", help="report drift, change nothing")
    ap.add_argument("--authors", type=Path, default=AUTHORS)
    ap.add_argument("--overrides", type=Path, default=OVERRIDES)
    a = ap.parse_args(argv)
    writes, deletes, problems = plan(a.authors, load_overrides(a.overrides))
    for p in problems:
        print(f"warning: {p}", file=sys.stderr)
    drift = sorted({str(p.relative_to(a.authors)) for p in [*writes, *deletes]})
    if a.check:
        if drift:
            print("owner overrides not applied to: " + ", ".join(drift), file=sys.stderr)
            print("fix: python3 tools/xerj-code/hub/candidates/owner_overrides.py", file=sys.stderr)
            return 1
        print("owner overrides: all applied")
        return 0
    for p, text in writes.items():
        p.write_text(text, encoding="utf-8")
    for p in deletes:
        os.remove(p)
    print(f"owner overrides: {len(writes)} file(s) written, {len(deletes)} removed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
