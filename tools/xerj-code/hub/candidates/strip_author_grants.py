#!/usr/bin/env python3
"""Remove fabricated author_grant blocks from candidates/authors records.

The outreach pipeline used to attach an ``author_grant`` block (usually
``{"terms": "MIT"}``) to records whose licence review said GPL/AGPL/MPL,
citing as its evidence the same email receipt that recorded the inclusion
reply (#2791, #2755). A consent reply only ever established inclusion under
the repository's licence; no reply granted a second, more permissive one.
validate_hub.py rejects the field, so a refresh that still carries it fails
``corpus-hub validate``; run this script on the refresh branch to fix it:

    python3 tools/xerj-code/hub/candidates/strip_author_grants.py

Idempotent: a clean tree is left byte-identical. Two shapes are handled,
matching where the pipeline wrote the field:
- ledger rows (CONTRIBUTORS*.jsonl): licence.author_grant, one JSON object
  per line; changed lines are re-dumped with the pipeline's own separators,
  untouched lines pass through verbatim;
- per-corpus manifests (authors/*.json): repos[0].review.author_grant as a
  pretty-printed block, removed textually so the rest of the file keeps the
  generator's exact formatting.

Stdlib only. Exits non-zero if a file mentions author_grant in a shape this
script does not recognize, so a new variant fails loudly instead of silently
surviving the guard.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent

# The block as the pipeline writes it: a flat object, pretty-printed with the
# key on its own line, preceded by a comma on the previous key's line.
BLOCK = re.compile(r',[ \t]*\n([ \t]*)"author_grant": \{[^{}]*?\n[ \t]*\}')


def strip_ledgers(authors: Path) -> int:
    stripped = 0
    for path in sorted(authors.glob("CONTRIBUTORS*.jsonl")):
        out: list[str] = []
        for line in path.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                out.append(line)
                continue
            row = json.loads(line)
            lic = row.get("licence")
            if isinstance(lic, dict) and "author_grant" in lic:
                del lic["author_grant"]
                stripped += 1
                out.append(json.dumps(row, ensure_ascii=False))
            else:
                out.append(line)  # verbatim: untouched rows keep their bytes
        path.write_text("\n".join(out) + "\n", encoding="utf-8")
    return stripped


def strip_manifests(authors: Path) -> int:
    stripped = 0
    for path in sorted(authors.glob("*.json")):
        text = path.read_text(encoding="utf-8")
        if '"author_grant"' not in text:
            continue
        new, n = BLOCK.subn("", text)
        if n != 1 or '"author_grant"' in new:
            print(f"{path}: author_grant present in an unrecognized shape "
                  f"({n} block match(es)); refusing to strip blindly", file=sys.stderr)
            sys.exit(1)
        json.loads(new)  # must still parse
        path.write_text(new, encoding="utf-8")
        stripped += 1
    return stripped


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--authors", type=Path, default=HERE / "authors",
                    help="candidates/authors directory (default: the one next to this script)")
    authors = ap.parse_args(argv).authors
    if not authors.is_dir():
        print(f"no candidates/authors directory at {authors}", file=sys.stderr)
        return 1
    led = strip_ledgers(authors)
    man = strip_manifests(authors)
    print(f"author_grant removed: {led} ledger row(s), {man} manifest(s); "
          f"the tree is clean when validate_hub.py passes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
