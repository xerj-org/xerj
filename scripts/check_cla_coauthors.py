#!/usr/bin/env python3
"""CLA check for Co-authored-by trailers (issue #269).

cla-bot resolves signatures by matching commit *authors* against
.contributors. When a maintainer re-lands a fork PR — squashing the
contributor's commits onto an xerj-org branch and crediting them with a
Co-authored-by trailer — every commit author is xerj-org, so the gate goes
green even though the person whose work it is has never signed (live
instance: PR #248, the re-land of buger's #166). This check closes that
hole: every Co-authored-by trailer on a PR must resolve to a login listed
in .contributors.

An email resolves to a login by, in order:

 1. GitHub noreply address — ``[id+]login@users.noreply.github.com``.
 2. This repo's own commit history — ``GET /repos/{repo}/commits?author=<email>``
    returns the login GitHub itself attributes that email to. The CLA
    signature flow ("open a PR adding your username to .contributors from
    your own account") lands a commit authored with the signer's email, so
    signing makes the signer resolvable as a side effect.

Anything unresolvable FAILS the gate, deliberately: an identity the gate
cannot attribute is exactly what it exists to flag.

Field-report carve-out (issue: the agent field-report loop was dead)
-------------------------------------------------------------------

A pull request whose ENTIRE diff is agent field reports — new markdown files
under ``user-feedback/16-agent-field-reports/`` and nothing else — is exempt
from this trailer gate. That folder is documentation of the *experience* of
using XERJ (see its README and https://xerj.org/llms.txt); requiring a CLA to
add one file there is exactly the friction that kept the folder empty. The
exemption is deliberately narrow: it needs every changed path to be a
field-report ``*.md``, so bundling any code, CI, or other-docs change with a
field report re-arms the full gate — a mixed PR must never slip a code change in
under a field report. ``README.md`` in that folder is NOT a field report and
does not qualify. See :func:`is_field_report_only`.

Usage (CI):
  check_cla_coauthors.py --pr <number> [--changed-files "<paths>"]
    Runs the trailer gate. If --changed-files is a field-report-only set, the
    gate is skipped (exempt) and this exits 0 without a network lookup.
  check_cla_coauthors.py --is-field-report-only --changed-files "<paths>"
    Prints nothing; exits 0 iff the paths are a field-report-only set, else 1.
    Used by the separate CI job that provides `verification/cla-signed` for
    strictly field-report-only pull requests.
  check_cla_coauthors.py --is-self-cla-signature --pr <n> --base-sha <sha> --head-sha <sha>
    Exits 0 iff the PR, evaluated at exactly --head-sha, is a CLA signature by
    its own author: the only change is `.contributors` gaining the author's own
    login, every commit is authored by the PR author, and no Co-authored-by
    trailer names an unsigned contributor. Everything (changed files, commits,
    both `.contributors` versions) is read pinned to --base-sha...--head-sha, so
    the status the CI job posts on --head-sha describes that commit and no
    other. Used by the same exemption job, because cla-bot reads
    `.contributors` from `main` and so can never pass a signature PR itself.
  Needs `gh` and GH_TOKEN for --pr; repo comes from GITHUB_REPOSITORY or --repo.

Tests: scripts/test_cla_coauthors.py
"""

import argparse
import base64
import json
import os
import pathlib
import re
import subprocess
import sys
import urllib.parse

REPO_ROOT = pathlib.Path(__file__).resolve().parent.parent

# git trailer keys are case-insensitive; GitHub emits "Co-authored-by".
_COAUTHOR_RE = re.compile(
    r"^\s*co-authored-by:\s*(?P<name>[^<]*?)\s*<(?P<email>[^>]+)>\s*$",
    re.IGNORECASE | re.MULTILINE,
)

_NOREPLY_RE = re.compile(
    r"^(?:\d+\+)?(?P<login>[^@+]+)@users\.noreply\.github\.com$", re.IGNORECASE
)


def parse_coauthors(message):
    """All Co-authored-by trailers in a commit message as (name, email)."""
    return [
        (m.group("name"), m.group("email").strip())
        for m in _COAUTHOR_RE.finditer(message)
    ]


def noreply_login(email):
    """Login embedded in a GitHub noreply address, or None."""
    m = _NOREPLY_RE.match(email.strip())
    return m.group("login") if m else None


# The one directory agent field reports live in. Kept in lock-step with
# xerj-autoindex's `feedback::FIELD_REPORT_DIR`; the `xerj feedback` command
# writes reports here and the carve-out below recognises them here.
FIELD_REPORT_DIR = "user-feedback/16-agent-field-reports/"


def is_field_report_only(paths):
    """True iff `paths` is a non-empty set of ONLY field-report markdown files.

    A field report is a ``*.md`` file directly inside FIELD_REPORT_DIR — not a
    nested subdirectory, and not the folder's own ``README.md`` (which is the
    template, not a report). An empty diff is NOT field-report-only: "nothing
    changed" must never read as "exempt". Any single non-qualifying path makes
    the whole set non-exempt, so a code change bundled with a field report gets
    the full gate.
    """
    paths = [p for p in paths if p.strip()]
    if not paths:
        return False
    for p in paths:
        p = p.strip()
        if not p.startswith(FIELD_REPORT_DIR):
            return False
        rest = p[len(FIELD_REPORT_DIR):]
        if "/" in rest:  # a nested subdirectory, not a direct field report
            return False
        if not rest.endswith(".md"):
            return False
        if rest == "README.md":  # the folder template is not a field report
            return False
    return True


# A GitHub login: ASCII alphanumerics and single hyphens, at most 39 chars.
# Deliberately ASCII-only — str.lower() folds some non-ASCII code points onto
# ASCII letters (e.g. KELVIN SIGN U+212A -> "k"), which must not count as a
# case-insensitive match for a real login.
_LOGIN_RE = re.compile(r"^[A-Za-z0-9](?:[A-Za-z0-9]|-(?=[A-Za-z0-9])){0,38}$")

# A signature PR has one or a handful of commits. Anything bigger is not what
# this exemption is for, and a cap keeps the commit list provably complete.
MAX_SIGNATURE_COMMITS = 20


def _is_login(value):
    return isinstance(value, str) and _LOGIN_RE.match(value) is not None


def is_self_cla_signature(
    files,
    base_contributors,
    head_contributors,
    pr_author,
    commits,
    resolve=lambda _email: None,
):
    """True iff the PR is its own author signing the CLA, and nothing else.

    cla-bot reads `.contributors` from `main`, so a PR whose sole purpose is
    signing the CLA (adding the PR author's own login to `.contributors`) can
    never pass cla-bot before it is merged. This predicate lets the trusted
    `pull_request_target` job satisfy `verification/cla-signed` for exactly
    that PR shape.

    files: the PR's changed-file entries, as GitHub's compare/files API returns
      them ({"filename", "status", "previous_filename"?}), pinned to the head
      SHA being judged.
    base_contributors / head_contributors: parsed `.contributors` at the merge
      base and at the head SHA.
    commits: [{"sha", "message", "author_login"}] — every commit in the PR.

    Rejects a PR that:
      - changes any file other than `.contributors`, or reaches it by a rename
        or copy (which would hide the deletion of the source path), or adds or
        deletes it rather than modifying it;
      - removes or alters any existing `.contributors` entry;
      - adds anything other than exactly ONE entry, that entry being a valid
        GitHub login equal (ASCII case-insensitively) to the PR author;
      - comes from an author who is already signed (nothing to sign);
      - has no commits, or any commit not authored by the PR author; or
      - carries a Co-authored-by trailer that is unresolvable or unsigned.
    """
    if not isinstance(files, list) or len(files) != 1:
        return False
    f = files[0]
    if not isinstance(f, dict):
        return False
    if f.get("filename") != ".contributors" or f.get("status") != "modified":
        return False
    if f.get("previous_filename"):
        return False

    if not _is_login(pr_author):
        return False
    author = pr_author.lower()

    for lst in (base_contributors, head_contributors):
        if not isinstance(lst, list) or not all(isinstance(c, str) for c in lst):
            return False
    if "dependabot[bot]" not in head_contributors:
        return False
    if any(c.lower() == author for c in base_contributors):
        return False  # already signed: there is nothing to sign

    # head must be exactly base plus one entry (multiset), entries untouched.
    remaining = list(head_contributors)
    for c in base_contributors:
        try:
            remaining.remove(c)
        except ValueError:
            return False  # an existing entry was removed or altered
    if len(remaining) != 1:
        return False
    added = remaining[0]
    if not _is_login(added) or added.lower() != author:
        return False

    if not commits or len(commits) > MAX_SIGNATURE_COMMITS:
        return False
    for commit in commits:
        login = commit.get("author_login")
        if not _is_login(login) or login.lower() != author:
            return False
    if check_commits(commits, head_contributors, resolve):
        return False
    return True


def check_commits(commits, contributors, resolve):
    """Return a list of problem strings; empty means the gate passes.

    commits: [{"sha": ..., "message": ...}]
    contributors: logins from .contributors
    resolve: email -> login-or-None (network lookup; injectable for tests)
    """
    signed = {c.lower() for c in contributors}
    resolved = {}  # email -> login or None, so each email is looked up once
    problems = []
    for commit in commits:
        sha = commit["sha"][:8]
        for name, email in parse_coauthors(commit["message"]):
            if email not in resolved:
                resolved[email] = noreply_login(email) or resolve(email)
            login = resolved[email]
            if login is None:
                problems.append(
                    f"{sha}: cannot attribute Co-authored-by '{name} <{email}>' "
                    "to any GitHub login (not a noreply address, and no commit "
                    "in this repo is authored with it). Ask them to sign per "
                    "CLA.md — the signature PR itself makes their email "
                    "resolvable — or use their @users.noreply.github.com "
                    "address in the trailer."
                )
            elif login.lower() not in signed:
                problems.append(
                    f"{sha}: Co-authored-by '{name} <{email}>' is GitHub user "
                    f"'{login}', who has not signed the CLA (not in "
                    ".contributors). See CLA.md for the one-time signature "
                    "flow."
                )
    return problems


def _gh_api(path):
    out = subprocess.run(
        ["gh", "api", "--paginate", path],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    # --paginate concatenates JSON arrays; parse them all.
    items = []
    dec = json.JSONDecoder()
    i = 0
    while i < len(out):
        chunk, end = dec.raw_decode(out, i)
        items.extend(chunk if isinstance(chunk, list) else [chunk])
        i = end
        while i < len(out) and out[i].isspace():
            i += 1
    return items


def make_repo_resolver(repo):
    """email -> login via the repo's own commit history."""

    def resolve(email):
        q = urllib.parse.quote(email)
        commits = _gh_api(f"repos/{repo}/commits?author={q}&per_page=1")
        for c in commits:
            author = c.get("author") or {}
            if author.get("login"):
                return author["login"]
        return None

    return resolve


def _split_paths(raw):
    """Changed-file paths from a --changed-files value (whitespace/newline sep)."""
    return [] if raw is None else raw.split()


def _gh_api_one(path):
    """A single (non-paginated) API response object."""
    out = subprocess.run(
        ["gh", "api", path], check=True, capture_output=True, text=True
    ).stdout
    return json.loads(out)


def _fetch_contributors_at(repo, sha):
    """`.contributors` at commit `sha`, parsed as pure JSON data via the
    contents API (never checking out or executing PR code). None on any
    failure, including a file over the contents API's inline size limit."""
    try:
        item = _gh_api_one(
            f"repos/{repo}/contents/.contributors?ref={urllib.parse.quote(sha)}"
        )
        if not isinstance(item, dict) or item.get("encoding") != "base64":
            return None
        raw = base64.b64decode(item["content"]).decode("utf-8")
        return json.loads(raw)
    except (subprocess.CalledProcessError, KeyError, ValueError, TypeError):
        return None


_SHA_RE = re.compile(r"^[0-9a-f]{40}$")


def _self_cla_signature_main(args):
    """--is-self-cla-signature: judge the PR at exactly args.head_sha."""
    if args.pr is None or not args.base_sha or not args.head_sha:
        print("--is-self-cla-signature needs --pr, --base-sha and --head-sha",
              file=sys.stderr)
        return 2
    if not (_SHA_RE.match(args.base_sha) and _SHA_RE.match(args.head_sha)):
        return 1
    try:
        pr_info = _gh_api_one(f"repos/{args.repo}/pulls/{args.pr}")
        # A stale event (the PR was pushed again since): its own run judges
        # the new head. Never post a verdict about a SHA we did not read.
        if (pr_info.get("head") or {}).get("sha") != args.head_sha:
            print("PR head moved since this event; not judging a stale SHA.")
            return 1
        if pr_info.get("state") != "open":
            return 1
        pr_author = (pr_info.get("user") or {}).get("login") or ""

        # Pinned to base...head: the files and commits are those of head_sha,
        # not whatever the PR points at by the time the API is read.
        cmp = _gh_api_one(
            f"repos/{args.repo}/compare/{args.base_sha}...{args.head_sha}"
        )
        raw_commits = cmp.get("commits") or []
        if cmp.get("total_commits") != len(raw_commits):
            return 1  # truncated list: cannot prove every commit was checked
        merge_base = (cmp.get("merge_base_commit") or {}).get("sha") or ""
        if not _SHA_RE.match(merge_base):
            return 1
        files = cmp.get("files")
        commits = [
            {
                "sha": c["sha"],
                "message": c["commit"]["message"],
                "author_login": (c.get("author") or {}).get("login"),
            }
            for c in raw_commits
        ]
    except (subprocess.CalledProcessError, KeyError, ValueError, TypeError):
        return 1

    base_contributors = _fetch_contributors_at(args.repo, merge_base)
    head_contributors = _fetch_contributors_at(args.repo, args.head_sha)
    if base_contributors is None or head_contributors is None:
        return 1
    ok = is_self_cla_signature(
        files,
        base_contributors,
        head_contributors,
        pr_author,
        commits,
        resolve=make_repo_resolver(args.repo),
    )
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--pr", type=int, help="pull request number")
    ap.add_argument(
        "--repo",
        default=os.environ.get("GITHUB_REPOSITORY", "xerj-org/xerj"),
        help="owner/name (default: $GITHUB_REPOSITORY or xerj-org/xerj)",
    )
    ap.add_argument(
        "--changed-files",
        help="whitespace/newline-separated changed paths in the PR. When this is "
        "a field-report-only set, the trailer gate is skipped (exempt).",
    )
    ap.add_argument(
        "--is-field-report-only",
        action="store_true",
        help="predicate mode: exit 0 iff --changed-files is a field-report-only "
        "set, else 1. Prints nothing; used by the exemption CI job.",
    )
    ap.add_argument(
        "--is-self-cla-signature",
        action="store_true",
        help="predicate mode: exit 0 iff the PR, at --head-sha, only adds the PR "
        "author's own login to .contributors, else 1. Used by the exemption CI job.",
    )
    ap.add_argument("--base-sha", help="PR base SHA (--is-self-cla-signature)")
    ap.add_argument("--head-sha", help="PR head SHA to judge (--is-self-cla-signature)")
    args = ap.parse_args()

    paths = _split_paths(args.changed_files)

    # Predicate mode: no PR, no network — just answer the path question.
    if args.is_field_report_only:
        return 0 if is_field_report_only(paths) else 1

    if args.is_self_cla_signature:
        return _self_cla_signature_main(args)

    if args.pr is None:
        ap.error("--pr is required unless --is-field-report-only is given")

    # Carve-out: a field-report-only PR skips the trailer gate entirely. The
    # predicate is a guard here, so check_commits() stays untouched and every
    # mixed or non-field-report PR runs the full check below.
    if args.changed_files is not None and is_field_report_only(paths):
        print(
            f"CLA co-author check EXEMPT for PR #{args.pr}: the diff is "
            f"{len(paths)} field-report file(s) under {FIELD_REPORT_DIR} and "
            "nothing else."
        )
        return 0

    contributors = json.loads((REPO_ROOT / ".contributors").read_text())
    raw = _gh_api(f"repos/{args.repo}/pulls/{args.pr}/commits?per_page=100")
    commits = [{"sha": c["sha"], "message": c["commit"]["message"]} for c in raw]

    problems = check_commits(commits, contributors, make_repo_resolver(args.repo))
    if problems:
        print(f"CLA co-author check FAILED for PR #{args.pr}:", file=sys.stderr)
        for p in problems:
            print(f"  - {p}", file=sys.stderr)
        return 1
    trailers = sum(len(parse_coauthors(c["message"])) for c in commits)
    print(
        f"CLA co-author check passed for PR #{args.pr}: "
        f"{len(commits)} commit(s), {trailers} Co-authored-by trailer(s), "
        "all attributable to signed contributors."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
