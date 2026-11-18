"""Install the pinned skill library into a home folder's .claude/skills.

    python3 -I install_fixture.py SOURCE_DIR HOME_DIR [--list FILE]

Every folder under SOURCE_DIR that holds a SKILL.md with a `name:` in its
frontmatter becomes HOME_DIR/.claude/skills/<name>/, the flat layout a person
gets by installing skills one by one. The library also ships the same skill at
two depths (a plugin copy and a flat copy) and a `.gemini/skills` tree of
links into itself, so: `.gemini` is skipped, links are not followed, the
shortest path wins when two folders share a name, and a skill folder is copied
without any skill folder nested inside it. `--list` writes one line per
installed skill: name, source path, description (tab separated).
Standard library only.
"""

import os
import re
import shutil
import sys

NAME = re.compile(r"^name:\s*[\"']?([^\"'\n]+?)[\"']?\s*$", re.M)
DESC = re.compile(r"^description:\s*(.*)$", re.M)


def frontmatter(text):
    if not text.startswith("---"):
        return None
    end = text.find("\n---", 3)
    return text[3:end] if end > 0 else None


def description(block):
    m = DESC.search(block)
    if not m:
        return ""
    value = m.group(1).strip()
    if value in (">", "|", ">-", "|-", ""):
        rest = block[m.end():].splitlines()
        lines = []
        for line in rest[1:] if rest and not rest[0].strip() else rest:
            if line[:1] in (" ", "\t"):
                lines.append(line.strip())
            elif line.strip():
                break
        value = " ".join(lines)
    return value.strip("\"'")


def skills(source):
    found = {}
    for dirpath, dirnames, filenames in os.walk(source):
        rel = os.path.relpath(dirpath, source)
        if rel.split(os.sep)[0] == ".gemini":
            dirnames[:] = []
            continue
        dirnames[:] = sorted(d for d in dirnames if not os.path.islink(os.path.join(dirpath, d)))
        path = os.path.join(dirpath, "SKILL.md")
        if "SKILL.md" not in filenames or os.path.islink(path):
            continue
        with open(path, encoding="utf-8", errors="replace") as fh:
            block = frontmatter(fh.read())
        m = NAME.search(block or "")
        if not m:
            continue
        name = m.group(1).strip()
        key = (rel.count(os.sep), rel)
        if name not in found or key < found[name][0]:
            found[name] = (key, rel, description(block))
    return {n: (rel, d) for n, (_, rel, d) in found.items()}


def nested_skill():
    def ignore(dirpath, names):
        return [
            n for n in names
            if os.path.islink(os.path.join(dirpath, n))
            or (os.path.isdir(os.path.join(dirpath, n))
                and os.path.exists(os.path.join(dirpath, n, "SKILL.md")))
        ]
    return ignore


def main(argv):
    if len(argv) not in (2, 4) or (len(argv) == 4 and argv[2] != "--list"):
        sys.exit(__doc__)
    source, home = argv[0], argv[1]
    target = os.path.join(home, ".claude", "skills")
    os.makedirs(target, exist_ok=True)
    found = skills(source)
    for name, (rel, _) in sorted(found.items()):
        if os.path.basename(name) != name or name in (".", ".."):
            continue  # a name that is not one folder name would land outside
        shutil.copytree(os.path.join(source, rel), os.path.join(target, name),
                        ignore=nested_skill())
    if len(argv) == 4:
        with open(argv[3], "w", encoding="utf-8") as out:
            for name, (rel, desc) in sorted(found.items()):
                out.write(f"{name}\t{rel}\t{' '.join(desc.split())}\n")
    print(f"installed {len(found)} skills into {target}")


if __name__ == "__main__":
    main(sys.argv[1:])
