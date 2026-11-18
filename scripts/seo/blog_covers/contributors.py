"""Build the contributor mosaic for /blog/100-contributors.

    python3 scripts/seo/blog_covers/contributors.py   # needs gh and network

Reads GitHub's contributor list for xerj-org/xerj (anonymous entries
included), leaves out the XERJ org account, its noreply address and
Dependabot, and writes:

    data/xerj-contributors.json             who is in the mosaic, in order
    landing/blog/covers/100-contributors-people.webp
                                            10 x 10 avatars, most commits
                                            first, ink-on-paper duotone

An anonymous entry (commits from an email with no GitHub account) has no
avatar, so it gets an ink tile with a yellow @. poster.py puts the same
mosaic on the cover. Pillow is the only dependency (see README).
"""

from __future__ import annotations

import io
import json
import pathlib
import subprocess
import urllib.request
from datetime import datetime

from PIL import Image, ImageDraw, ImageFont, ImageOps

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parents[2]
DATA = HERE / "data" / "xerj-contributors.json"
OUT = ROOT / "landing" / "blog" / "covers" / "100-contributors-people.webp"
LEAVE_OUT = {"xerj-org", "noreply@xerj.org", "dependabot[bot]"}
INK, PAPER, YELLOW = "#1A1A1A", "#FAFAF7", "#FFC400"
TILE, GAP, N = 72, 6, 10


def contributors() -> list[dict]:
    raw = subprocess.run(
        ["gh", "api", "--paginate", "--slurp",
         "repos/xerj-org/xerj/contributors?anon=1&per_page=100"],
        check=True, capture_output=True, text=True).stdout
    rows = [r for page in json.loads(raw) for r in page]
    return [r for r in rows if (r.get("login") or r.get("email")) not in LEAVE_OUT]


def main() -> None:
    rows = contributors()
    font = ImageFont.truetype(str(HERE / "fonts" / "JetBrainsMono-wght.ttf"), 32)
    side = N * TILE + (N - 1) * GAP
    rows_out = (len(rows) + N - 1) // N
    im = Image.new("RGB", (side, rows_out * TILE + (rows_out - 1) * GAP), PAPER)
    people = []
    for k, r in enumerate(rows):
        x, y = (k % N) * (TILE + GAP), (k // N) * (TILE + GAP)
        if r["type"] == "Anonymous":
            t = Image.new("RGB", (TILE, TILE), INK)
            ImageDraw.Draw(t).text((TILE / 2, TILE / 2 - 2), "@", fill=YELLOW, font=font, anchor="mm")
            people.append({"anonymous": True, "commits": r["contributions"]})
        else:
            url = r["avatar_url"] + ("&" if "?" in r["avatar_url"] else "?") + "s=96"
            with urllib.request.urlopen(url, timeout=30) as f:
                a = Image.open(io.BytesIO(f.read())).convert("RGB").resize((TILE, TILE), Image.LANCZOS)
            g = ImageOps.autocontrast(ImageOps.grayscale(a), cutoff=1)
            t = ImageOps.colorize(g, black=INK, white=PAPER)
            people.append({"login": r["login"], "type": r["type"], "commits": r["contributions"]})
        im.paste(t, (x, y))
    im.save(OUT, quality=80, method=6)
    DATA.write_text(json.dumps({
        "_source": "GitHub REST API, GET /repos/xerj-org/xerj/contributors?anon=1, "
                   "without " + ", ".join(sorted(LEAVE_OUT)),
        "fetched": datetime.now().astimezone().isoformat(timespec="seconds"),
        "count": len(people), "people": people}, indent=1) + "\n")
    print(f"wrote {OUT.relative_to(ROOT)} ({len(people)} contributors, {OUT.stat().st_size:,} B)")


if __name__ == "__main__":
    main()
