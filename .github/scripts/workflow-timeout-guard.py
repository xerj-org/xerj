#!/usr/bin/env python3
"""Every GitHub Actions job in this repo must declare its own `timeout-minutes`.

Why this gate exists (#751 -> #770)
-----------------------------------
A job with no `timeout-minutes` inherits GitHub's default of 360 minutes. That
default is not a safety net here, it is the blast radius: several of this repo's
workflows serialise on a concurrency group that never auto-cancels --

    ci.yml            group `ci-CI-refs/heads/main`, cancel-in-progress: false
    deploy-pages.yml  group `pages-deploy`,          cancel-in-progress: false
    release-metrics   group `release-metrics`,       cancel-in-progress: false

-- so ONE hung job holds that group's slot for hours and every later run queues
behind it. That is not hypothetical: on 2026-08-25 the #751 engine deadlock ran
`build-test` into the 360-minute default and was killed by it (run 32796557309,
01:21:28 -> 07:21:44, while every other job in that run finished inside 18
minutes), and a second hang the same morning held the slot 11:28 -> 17:03 while
seven consecutive pushes to main were discarded having run ZERO jobs. #899 fixed
that particular deadlock; it did not shrink the 6-hour exposure of the other 22
jobs, which is what #770 asked for.

Bounding every job turns "main has no CI verdict for six hours" into "one job
fails in at most `MAX_TIMEOUT` minutes and `gh run rerun --failed` recovers it".

The rules
---------
1. Every job declares a job-level `timeout-minutes`.
2. The value is an integer in [1, MAX_TIMEOUT]. The upper bound is what makes
   the gate mean something: a job allowed 300 minutes would satisfy rule 1 and
   still hold a slot all afternoon.
3. Parser self-check: every block this script treats as a job must contain
   `runs-on:` or `uses:`. If a workflow file's shape ever drifts away from what
   the parser below assumes, that check fails loudly instead of the guard
   silently passing over jobs it never saw.

The one exemption is a job that calls a reusable workflow (`uses:` at job
level): GitHub rejects `timeout-minutes` on those outright, so the cap has to
live on the jobs inside the called workflow. This repo has none today.

Stdlib only, on purpose: this runs as a merge gate, and a gate that needs
`pip install` on a hosted runner is a gate that can fail for reasons that have
nothing to do with the tree (the same rule the SEO gate follows).

Usage:
    python3 .github/scripts/workflow-timeout-guard.py [--dir .github/workflows]
    python3 .github/scripts/workflow-timeout-guard.py --self-test
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# GitHub's own default is 360. Nothing in this repo has ever legitimately needed
# more than ~48 min (`Build + Test`, the whole workspace compiled and tested at
# --test-threads=2) or ~34 min (the Windows cross-compile in release.yml), both
# measured over every non-cancelled run of 2026-08-24..31. 90 leaves room for a
# cold dependency cache on the heaviest job while still being 4x below the
# default.
MAX_TIMEOUT = 90

# A job id: exactly two spaces of indent, a key, nothing else on the line.
JOB_RE = re.compile(r"^  ([A-Za-z_][A-Za-z0-9_.-]*):\s*(#.*)?$")
# A job-level key: exactly four spaces of indent. Step-level keys live at eight
# or more, so `timeout-minutes` on a *step* can never satisfy this gate -- which
# matters, because ci.yml already has one (build-test's 12-minute step-6 cap)
# and it does not bound the job.
JOB_KEY_RE = re.compile(r"^    ([A-Za-z_][A-Za-z0-9_.-]*):\s*(.*?)\s*(?:#.*)?$")


class ParseError(Exception):
    pass


def parse_jobs(text: str, where: str) -> list:
    """Return [(job_id, {job-level key: raw value})] for one workflow file.

    Deliberately a small line parser rather than a YAML load: see the module
    docstring. It relies on this repo's uniform 2-space workflow style, and
    rule 3 above is what detects it if that ever stops holding.
    """
    lines = text.split("\n")
    start = None
    for i, ln in enumerate(lines):
        if re.match(r"^jobs:\s*(#.*)?$", ln):
            start = i
            break
    if start is None:
        raise ParseError(f"{where}: no top-level `jobs:` block")

    jobs = []
    current = None
    for ln in lines[start + 1 :]:
        if not ln.strip() or ln.lstrip().startswith("#"):
            continue
        indent = len(ln) - len(ln.lstrip())
        if indent == 0:
            break  # back out to the next top-level key
        m = JOB_RE.match(ln)
        if m:
            current = {}
            jobs.append((m.group(1), current))
            continue
        if current is None:
            raise ParseError(f"{where}: content before the first job: {ln!r}")
        km = JOB_KEY_RE.match(ln)
        if km:
            current.setdefault(km.group(1), km.group(2))
    if not jobs:
        raise ParseError(f"{where}: `jobs:` block contains no jobs")
    return jobs


def check_workflow(path: Path, text: str) -> list:
    where = path.as_posix()
    problems = []
    for job_id, keys in parse_jobs(text, where):
        # Rule 3 first: if this is not really a job, everything else is noise.
        if "runs-on" not in keys:
            if "uses" in keys:
                # A job that calls a reusable workflow. GitHub REJECTS
                # `timeout-minutes` on those ("Unexpected value"), so requiring
                # one here would demand an invalid workflow; the cap has to go
                # on the jobs inside the called workflow instead.
                continue
            problems.append(
                f"{where}: job `{job_id}` has neither `runs-on:` nor `uses:` -- "
                f"the guard's parser is out of step with this file; fix the "
                f"parser rather than the workflow"
            )
            continue
        raw = keys.get("timeout-minutes")
        if raw is None:
            problems.append(
                f"{where}: job `{job_id}` has no `timeout-minutes:` and so "
                f"inherits GitHub's 360-minute default (see #770)"
            )
            continue
        try:
            value = int(raw)
        except ValueError:
            problems.append(
                f"{where}: job `{job_id}` has a non-integer "
                f"`timeout-minutes: {raw}`"
            )
            continue
        if not 1 <= value <= MAX_TIMEOUT:
            problems.append(
                f"{where}: job `{job_id}` has `timeout-minutes: {value}`, "
                f"outside the allowed 1..{MAX_TIMEOUT}"
            )
    return problems


def run(workflow_dir: Path) -> int:
    files = sorted(p for p in workflow_dir.iterdir() if p.suffix in (".yml", ".yaml"))
    if not files:
        print(f"error: no workflow files under {workflow_dir}", file=sys.stderr)
        return 1
    problems = []
    jobs_seen = 0
    for path in files:
        text = path.read_text(encoding="utf-8")
        try:
            jobs_seen += len(parse_jobs(text, path.as_posix()))
        except ParseError as exc:
            problems.append(str(exc))
            continue
        problems.extend(check_workflow(path, text))
    if problems:
        for p in problems:
            print(f"::error::{p}")
        print(
            f"\n{len(problems)} problem(s) across {len(files)} workflow file(s). "
            f"Every job needs `timeout-minutes:` (1..{MAX_TIMEOUT}); a job "
            f"without one can hold a non-cancelling concurrency slot for 6 hours.",
            file=sys.stderr,
        )
        return 1
    print(
        f"ok: {jobs_seen} job(s) across {len(files)} workflow file(s) "
        f"all declare timeout-minutes <= {MAX_TIMEOUT}"
    )
    return 0


# --------------------------------------------------------------------------
# Self-test: the guard is itself load-bearing logic, so it ships with fixtures
# for the ways it could be wrong -- missing a violation, inventing one, or
# accepting a STEP-level timeout as if it bounded the job.
# --------------------------------------------------------------------------
_GOOD = """\
name: X
on: [push]
jobs:
  alpha:
    name: A
    runs-on: ubuntu-latest
    timeout-minutes: 15
    steps:
      - run: echo hi
  beta:
    runs-on: ubuntu-latest
    timeout-minutes: 60
    strategy:
      matrix:
        os: [ubuntu-latest]
    steps:
      - name: s
        timeout-minutes: 5
        run: |
          echo "  not-a-job: true"
"""

_MISSING = _GOOD.replace("    timeout-minutes: 15\n", "")
_STEP_ONLY = _GOOD.replace("    timeout-minutes: 60\n", "")
_TOO_BIG = _GOOD.replace("    timeout-minutes: 15", "    timeout-minutes: 300")
_NOT_A_JOB = _GOOD.replace("    runs-on: ubuntu-latest\n    timeout-minutes: 15\n", "")
_REUSABLE = _GOOD.replace(
    "    runs-on: ubuntu-latest\n    timeout-minutes: 15\n",
    "    uses: ./.github/workflows/other.yml\n",
)


def self_test() -> int:
    fake = Path("fixture.yml")
    cases = [
        ("good", _GOOD, 0),
        ("job missing timeout-minutes", _MISSING, 1),
        ("only a step-level timeout-minutes", _STEP_ONLY, 1),
        ("timeout above the cap", _TOO_BIG, 1),
        ("block that is not a job", _NOT_A_JOB, 1),
        ("reusable-workflow call is exempt", _REUSABLE, 0),
    ]
    failures = 0
    for label, text, want in cases:
        got = len(check_workflow(fake, text))
        status = "ok " if got == want else "FAIL"
        if got != want:
            failures += 1
        print(f"{status} {label}: expected {want} problem(s), got {got}")
    # The parser must see exactly the two jobs, not the `run:` block's decoy.
    ids = [j for j, _ in parse_jobs(_GOOD, "fixture.yml")]
    if ids != ["alpha", "beta"]:
        print(f"FAIL parser found jobs {ids}, expected ['alpha', 'beta']")
        failures += 1
    else:
        print("ok  parser finds exactly the real jobs")
    print("self-test:", "PASS" if failures == 0 else f"{failures} FAILURE(S)")
    return 1 if failures else 0


def main() -> int:
    ap = argparse.ArgumentParser(description="Job-timeout guard for .github/workflows")
    ap.add_argument("--dir", default=".github/workflows")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    return run(Path(args.dir))


if __name__ == "__main__":
    sys.exit(main())
