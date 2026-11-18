#!/usr/bin/env python3
"""Generate the public Agent Skills Discovery v0.2.0 index (stdlib only).

The published SKILL.md is the source, not a generated copy. Only the explicit
PUBLIC_SKILLS list is indexed; internal tools/** skills are not exported.
Frontmatter uses a plain name and a JSON-quoted, single-line description (a
small YAML subset), so metadata needs no YAML dependency or duplicated data.

Reference: https://github.com/cloudflare/agent-skills-discovery-rfc
Run with --write to regenerate, or --check to detect stale/missing output.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import sys

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
PUBLIC_SKILLS = ("xerj",)
DISCOVERY = ".well-known/agent-skills"
SCHEMA = "https://schemas.agentskills.io/discovery/0.2.0/schema.json"


def skill_entry(path: pathlib.Path) -> dict[str, str]:
    raw = path.read_bytes()
    lines = raw.decode("utf-8").splitlines()
    if len(lines) < 5 or lines[0] != "---":
        raise ValueError(f"{path}: missing skill frontmatter")
    try:
        closing = lines.index("---", 1)
    except ValueError as exc:
        raise ValueError(f"{path}: unclosed skill frontmatter") from exc
    fields = {}
    for line in lines[1:closing]:
        key, sep, value = line.partition(":")
        if not sep or key not in {"name", "description"} or key in fields:
            raise ValueError(f"{path}: expected unique name and description fields")
        fields[key] = value.strip()
    name = fields.get("name", "")
    if len(name) > 64 or not re.fullmatch(r"[a-z0-9]+(?:-[a-z0-9]+)*", name):
        raise ValueError(f"{path}: invalid skill name")
    if name != path.parent.name:
        raise ValueError(f"{path}: name must match its directory")
    description = json.loads(fields.get("description", "null"))
    if (not isinstance(description, str) or not description.strip()
            or len(description) > 1024 or "\n" in description or "\r" in description):
        raise ValueError(f"{path}: description must be one JSON-quoted line, 1-1024 characters")
    if not "\n".join(lines[closing + 1:]).strip():
        raise ValueError(f"{path}: missing skill instructions")
    return {
        "name": name,
        "type": "skill-md",
        "description": description,
        "url": f"/{DISCOVERY}/{name}/SKILL.md",
        "digest": "sha256:" + hashlib.sha256(raw).hexdigest(),
    }


def render(landing: pathlib.Path) -> bytes:
    entries = [skill_entry(landing / DISCOVERY / name / "SKILL.md")
               for name in PUBLIC_SKILLS]
    return (json.dumps({"$schema": SCHEMA, "skills": entries}, indent=2,
                       ensure_ascii=False) + "\n").encode("utf-8")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--write", action="store_true")
    mode.add_argument("--check", action="store_true")
    args = parser.parse_args(argv)
    landing = REPO_ROOT / "landing"
    target = landing / DISCOVERY / "index.json"
    try:
        expected = render(landing)
        if args.write:
            target.write_bytes(expected)
        elif not target.is_file() or target.read_bytes() != expected:
            print("skill index missing or stale; run python3 scripts/seo/build_skills.py --write",
                  file=sys.stderr)
            return 1
    except (OSError, UnicodeError, ValueError) as exc:
        print(f"build_skills: {exc}", file=sys.stderr)
        return 1
    print("ok   public skill index matches its SKILL.md metadata and bytes")
    return 0


if __name__ == "__main__":
    sys.exit(main())
