#!/usr/bin/env python3
"""Compare batched dates and generated heads with the old per-file Git oracle.

Run from a full checkout: python3 scripts/seo/verify_date_batch.py
Read-only; prints measured times/process counts and exits nonzero on drift.
"""

import json
import os
import subprocess
import time
from unittest import mock

import fix_heads
import urlmap


class PerFileDates(urlmap.DateSource):
    """The pre-#3226 implementation, kept here as an independent Git oracle."""

    def _git_date(self, path, first=False):
        if not self.git_ok:
            return None
        cmd = ["git", "-C", str(self.repo_root), "log",
               "--date=format-local:%Y-%m-%d", "--format=%cd"]
        if not first:
            cmd.append("-1")
        cmd.extend(["--", str(path)])
        result = subprocess.run(cmd, capture_output=True, text=True, timeout=30,
                                env={**os.environ, "TZ": "UTC"}, check=True)
        days = result.stdout.splitlines()
        return days[-1 if first else 0] if days else None


def evaluate(cls, root, paths):
    start = time.perf_counter()
    with mock.patch.object(urlmap.subprocess, "run", wraps=subprocess.run) as run:
        dates = cls(root)
        values = [(dates.first_published(path), dates.last_modified(path)) for path in paths]
        heads = [fix_heads.transform(path.relative_to(root / "landing").as_posix(),
                                     path.read_text(encoding="utf-8"), dates, path)
                 for path in paths]
    return values, heads, dates.fallbacks, {
        "seconds": round(time.perf_counter() - start, 3),
        "git_processes": run.call_count,
        "mode": ("per_file_git" if cls is PerFileDates else
                 "rename_following" if dates._follow_renames else "batched"),
    }


def main():
    root = urlmap.repo_root()
    if not urlmap.DateSource(root).git_ok:
        raise SystemExit("This comparison requires a full Git checkout.")
    paths = [root / "landing" / rel for rel in urlmap.iter_pages(root / "landing")]
    batched = evaluate(urlmap.DateSource, root, paths)
    baseline = evaluate(PerFileDates, root, paths)
    mismatches = [str(path.relative_to(root)) for i, path in enumerate(paths)
                  if batched[0][i] != baseline[0][i] or batched[1][i] != baseline[1][i]]
    print(json.dumps({"pages": len(paths), "batched": batched[3], "per_file": baseline[3],
                      "mismatches": mismatches, "fallbacks_equal": batched[2] == baseline[2]},
                     indent=2))
    too_many = batched[3]["mode"] == "batched" and batched[3]["git_processes"] > 4
    return int(bool(mismatches) or batched[2] != baseline[2] or too_many)


if __name__ == "__main__":
    raise SystemExit(main())
