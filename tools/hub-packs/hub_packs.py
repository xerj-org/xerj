#!/usr/bin/env python3
"""Planning and re-pinning for the Corpus Hub v2 pack pipeline.

Drives `.github/workflows/hub-packs.yml` (plan §8, docs/CORPUS_HUB_V2_IMPLEMENTATION.md).
Stdlib only, so the workflow needs no `pip install` and the tests run anywhere.

Three jobs, all pure functions over the registry first and thin I/O second:

  plan    Which live corpora need a pack built?  A corpus is DUE when no published
          release `pack-<slug>-<pin8>` exists for its current pin (pin8 = the first
          8 hex of the manifest's FIRST repo sha, the pin the catalogue carries).
          Deciding from release state rather than from "which manifests changed in
          this push" is deliberate: a dropped, cancelled or failed run costs
          nothing, because the next run sees the same unreleased pin and builds it.

  repin   For corpora whose upstream moved: `git ls-remote <url> HEAD` per repo,
          and if the default-branch head differs from the manifest's `sha`, open a
          PULL REQUEST that moves the pin.  A human merges; the packs follow on
          merge (the `plan` above sees the new, unreleased pin).

  report  One row per corpus per run (`result-row` in each pack job, `report` at the
          end): REPORT.md + rows.jsonl under benchmarks/hub-backfill/ on corpus-hub,
          so a corpus whose build failed is listed with its error (spec §12.1).

Registry facts this relies on (tools/xerj-code/hub/):
  * `<slug>.json` is `{"corpus": slug, "cloned_at": ..., "repos": [{"repo", "url",
    "sha", "review": {...}, ...}]}`; the catalogue pin is `repos[0].sha`.
  * `backlog/backlog-100.json` rows carry `status` (live|candidate|...), `lane`
    (A = git manifest, B = harvested pack recipe) and `refresh`
    (pinned|weekly|daily).  Only live lane-A rows have a manifest this pipeline
    can build.
"""

from __future__ import annotations

import argparse
import dataclasses
import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys
from datetime import datetime, timezone
from typing import Callable, Iterable, Optional

SHA_RE = re.compile(r"^[0-9a-f]{40}$")
# Same rule as `valid_slug` in xerj-hub: a slug is joined into filesystem paths
# and release tags, so it must be a plain lower-case name.
SLUG_RE = re.compile(r"^[a-z0-9]([a-z0-9._-]*[a-z0-9])?$")

# `refresh` class -> catalogue `refresh_cadence_days` (plan §1: "7 for code, 1 for
# feeds").  `pinned` is code at a release tag: weekly cadence for the freshness
# warning, but never auto-re-pinned (see REPIN_CLASSES).
CADENCE_DAYS = {"daily": 1, "weekly": 7, "pinned": 7}

# The refresh classes the scheduled re-pin job may propose a new pin for.
# `pinned` is NOT among them: PROGRAM-100.md §3 says a pinned corpus is code at a
# release tag whose "pin is the promise", and comparing a release tag with the
# default-branch head would differ for every one of ~500 corpora every week.
# `--include-pinned` is the deliberate override.
REPIN_CLASSES = ("weekly", "daily")

BRANCH_PREFIX = "hub-repin/"
BOT_NAME = "xerj-org"
BOT_EMAIL = "xerj-org@users.noreply.github.com"


class RegistryError(Exception):
    """A manifest or backlog row this pipeline cannot use."""


# ── registry ────────────────────────────────────────────────────────────────


@dataclasses.dataclass(frozen=True)
class Repo:
    name: str
    url: str
    sha: str


@dataclasses.dataclass(frozen=True)
class Corpus:
    slug: str
    path: pathlib.Path  # the manifest file
    refresh: str  # pinned | weekly | daily
    repos: tuple[Repo, ...]

    @property
    def pin_sha(self) -> str:
        """The catalogue's pin: the first repo's sha (xerj-hub catalogue.rs)."""
        return self.repos[0].sha

    @property
    def pin8(self) -> str:
        return self.pin_sha[:8]

    @property
    def cadence_days(self) -> int:
        return CADENCE_DAYS.get(self.refresh, 7)


def load_manifest(path: pathlib.Path, refresh: str = "pinned") -> Corpus:
    """Parse one registry manifest, refusing anything the pack builder would."""
    try:
        data = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as e:
        raise RegistryError(f"{path.name}: unreadable ({e})") from e
    slug = data.get("corpus")
    if slug != path.stem or not isinstance(slug, str) or not SLUG_RE.match(slug):
        raise RegistryError(f"{path.name}: corpus {slug!r} must equal the file name")
    repos = []
    for i, r in enumerate(data.get("repos") or []):
        sha = r.get("sha", "")
        if not SHA_RE.match(sha):
            raise RegistryError(f"{path.name}: repos[{i}].sha is not a 40-hex commit")
        if not r.get("url") or not r.get("repo"):
            raise RegistryError(f"{path.name}: repos[{i}] lacks repo/url")
        repos.append(Repo(r["repo"], r["url"], sha))
    if not repos:
        raise RegistryError(f"{path.name}: no repos")
    return Corpus(slug, path, refresh, tuple(repos))


def load_live(
    registry: pathlib.Path, backlog: pathlib.Path
) -> tuple[list[Corpus], list[str]]:
    """Live lane-A corpora that have a usable manifest, plus warnings for the
    ones that are live but cannot be built (so a bad manifest is loud, not silent)."""
    rows = json.loads(backlog.read_text()).get("corpora", [])
    corpora: list[Corpus] = []
    warnings: list[str] = []
    for row in rows:
        if row.get("status") != "live" or row.get("lane") != "A":
            continue
        slug = row.get("slug", "")
        manifest = registry / f"{slug}.json"
        if not manifest.is_file():
            warnings.append(f"{slug}: live in the backlog but {manifest.name} is missing")
            continue
        try:
            corpora.append(load_manifest(manifest, row.get("refresh", "pinned")))
        except RegistryError as e:
            warnings.append(str(e))
    corpora.sort(key=lambda c: c.slug)
    return corpora, warnings


# ── plan: which packs need building ─────────────────────────────────────────


def pack_tag(slug: str, pin8: str) -> str:
    return f"pack-{slug}-{pin8}"


def released_tag(tags: Iterable[str], slug: str, pin8: str) -> Optional[str]:
    """The published release tag for this pin, or None.  When the publish step had
    to retag past a burned tag (GitHub's immutable releases never free one), the
    newest `-rN` wins."""
    base = pack_tag(slug, pin8)
    rx = re.compile(re.escape(base) + r"(?:-r(\d+))?")
    best: Optional[int] = None
    for t in tags:
        m = rx.fullmatch(t)
        if m:
            n = int(m.group(1) or 1)
            best = n if best is None else max(best, n)
    if best is None:
        return None
    return base if best == 1 else f"{base}-r{best}"


def is_released(tags: Iterable[str], slug: str, pin8: str) -> bool:
    """A published release exists for this pin (its tag, or a `-rN` retag)."""
    return released_tag(tags, slug, pin8) is not None


def plan(
    corpora: list[Corpus],
    published_tags: Iterable[str],
    slugs: Optional[set[str]] = None,
    force: bool = False,
    max_builds: int = 40,
) -> dict:
    """The build matrix.  `slugs` restricts to named corpora; `force` rebuilds a
    pin that is already released (the publish step then takes a `-rN` tag)."""
    tags = list(published_tags)
    due = []
    for c in corpora:
        if slugs is not None and c.slug not in slugs:
            continue
        if not force and is_released(tags, c.slug, c.pin8):
            continue
        due.append(
            {
                "slug": c.slug,
                "pin8": c.pin8,
                "tag": pack_tag(c.slug, c.pin8),
                "cadence_days": c.cadence_days,
            }
        )
    chosen = due[:max_builds]
    return {
        "include": chosen,
        "count": len(chosen),
        "due": len(due),
        "capped": len(due) > len(chosen),
    }


# ── repin: has upstream moved? ──────────────────────────────────────────────


@dataclasses.dataclass(frozen=True)
class Move:
    repo: str
    url: str
    old: str
    new: str


@dataclasses.dataclass
class Proposal:
    slug: str
    path: str  # repo-relative manifest path
    moves: list[Move]
    new_text: str
    pin_date: str
    proposal_id: str  # 8 hex, identifies THIS set of new shas
    branch: str
    title: str
    body: str

    def to_json(self) -> dict:
        return dataclasses.asdict(self)


GitHeadFn = Callable[[str], Optional[str]]


def git_head_of(url: str, allow_protocols: str = "https") -> Optional[str]:
    """`git ls-remote <url> HEAD`: the default-branch head, or None when the
    remote is unreachable or advertises no HEAD.

    The protocol allowlist is the point: a manifest URL is reviewed, but
    `ext::<command>` would run it, so git is told to speak `https` only (the
    tests widen it to `file`).  Never prompts for credentials."""
    env = dict(os.environ)
    env["GIT_ALLOW_PROTOCOL"] = allow_protocols
    env["GIT_TERMINAL_PROMPT"] = "0"
    scheme = url.split("://", 1)[0] if "://" in url else ""
    if scheme not in allow_protocols.split(":"):
        return None
    try:
        out = subprocess.run(
            ["git", "ls-remote", url, "HEAD"],
            capture_output=True,
            text=True,
            timeout=90,
            env=env,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if out.returncode != 0:
        return None
    for line in out.stdout.splitlines():
        sha, _, ref = line.partition("\t")
        if ref.strip() == "HEAD" and SHA_RE.match(sha):
            return sha
    return None


def _repo_span(text: str, name: str) -> tuple[int, int]:
    """The [start, end) of the JSON object text for repo `name` — from its
    `"repo": "<name>"` key to the next `"repo":` key (or the end).  Scoping the
    sha replacement to it is what keeps two repos pinned at the same commit apart."""
    m = re.search(r'"repo"\s*:\s*"' + re.escape(name) + r'"', text)
    if not m:
        raise RegistryError(f"repo {name!r} not found in the manifest text")
    nxt = re.search(r'"repo"\s*:', text[m.end() :])
    return m.start(), (m.end() + nxt.start() if nxt else len(text))


def rewrite_manifest(text: str, moves: list[Move], pin_date: str) -> str:
    """Move the pins and stamp the top-level `pin_date`, changing NOTHING else —
    the manifest is hand-formatted, so a JSON round-trip would reformat the whole
    file and bury the one-line change in diff noise."""
    for mv in moves:
        start, end = _repo_span(text, mv.repo)
        span = text[start:end]
        rx = re.compile(r'("sha"\s*:\s*")' + re.escape(mv.old) + r'(")')
        if len(rx.findall(span)) != 1:
            raise RegistryError(
                f"repo {mv.repo!r}: expected exactly one sha {mv.old[:8]}… to replace"
            )
        text = text[:start] + rx.sub(lambda m: m.group(1) + mv.new + m.group(2), span) + text[end:]

    stamp = f'"pin_date":"{pin_date}"'
    head = re.match(r'^(\{\s*"corpus"\s*:\s*"[^"]*"\s*,)', text)
    existing = re.search(r'"pin_date"\s*:\s*"[^"]*"', text)
    repos_at = text.find('"repos"')
    if existing and (repos_at < 0 or existing.start() < repos_at):
        text = text[: existing.start()] + stamp + text[existing.end() :]
    elif head:
        text = head.group(1) + stamp + "," + text[head.end() :]
    else:
        raise RegistryError("manifest does not start with {\"corpus\":...; cannot place pin_date")
    return text


def _compare_url(url: str, old: str, new: str) -> Optional[str]:
    m = re.match(r"^https://github\.com/([^/]+)/([^/]+?)(?:\.git)?/?$", url)
    return f"https://github.com/{m.group(1)}/{m.group(2)}/compare/{old[:12]}...{new[:12]}" if m else None


def title_for(c: Corpus, moves: list[Move]) -> str:
    first = moves[0]
    if len(c.repos) == 1:
        return f"hub: re-pin {c.slug} to {first.new[:8]}"
    more = f" (+{len(moves) - 1} more)" if len(moves) > 1 else ""
    return f"hub: re-pin {c.slug}: {first.repo} to {first.new[:8]}{more}"


def render_body(c: Corpus, moves: list[Move], pin_date: str) -> str:
    rows = []
    for mv in moves:
        cmp_url = _compare_url(mv.url, mv.old, mv.new)
        link = f"[compare]({cmp_url})" if cmp_url else "(no compare link: not a github.com URL)"
        rows.append(f"| `{mv.repo}` | {mv.url} | `{mv.old[:12]}` | `{mv.new[:12]}` | {link} |")
    first_moved = next((m for m in moves if m.repo == c.repos[0].name), None)
    catalogue_note = (
        f"The catalogue pin (the first repo's sha) moves to `{first_moved.new[:8]}`: merging "
        f"this builds and publishes `pack-{c.slug}-{first_moved.new[:8]}`, and clients see the "
        "new pin on their next `xerj hub sync`."
        if first_moved
        else "The catalogue pin (the FIRST repo's sha) does not change, so existing clients "
        "will not see a new pin from this PR alone; the pack content would be rebuilt only "
        "under a `-rN` release tag."
    )
    return "\n".join(
        [
            f"The upstream default branch of `{c.slug}` moved since the manifest was pinned "
            f"(refresh class `{c.refresh}`). This PR was opened by the scheduled "
            "`hub packs` re-pin job; **a human decides whether to merge it.**",
            "",
            "| repo | url | pinned | upstream HEAD | |",
            "|---|---|---|---|---|",
            *rows,
            "",
            catalogue_note,
            "",
            "Before merging:",
            "",
            "- [ ] Re-open each repo's licence file **at the new sha** and confirm the `review` "
            "block still holds (Gate G4). The block is carried over unchanged; its `at` date is "
            "the old review, not this one.",
            "- [ ] `files` / `bytes` are NOT recomputed by this PR; they describe the old checkout.",
            f"- [ ] `pin_date` is stamped `{pin_date}` (the day the pin moved).",
            "",
            "If upstream rewrote history (the old sha no longer resolves), withdraw the corpus "
            "instead of re-pinning it (PROGRAM-100.md §3).",
        ]
    )


def find_proposals(
    corpora: list[Corpus],
    classes: Iterable[str],
    head_of: GitHeadFn,
    today: str,
    only: Optional[set[str]] = None,
) -> tuple[list[Proposal], list[str]]:
    """Corpora in `classes` whose upstream HEAD differs from the pinned sha."""
    classes = set(classes)
    proposals: list[Proposal] = []
    notes: list[str] = []
    for c in corpora:
        if c.refresh not in classes or (only is not None and c.slug not in only):
            continue
        moves: list[Move] = []
        for r in c.repos:
            head = head_of(r.url)
            if head is None:
                notes.append(f"{c.slug}/{r.name}: cannot read HEAD of {r.url}; skipped")
            elif head != r.sha:
                moves.append(Move(r.name, r.url, r.sha, head))
        if not moves:
            continue
        try:
            new_text = rewrite_manifest(c.path.read_text(), moves, today)
            # The rewrite must be exactly the intended change, nothing more.
            _assert_only_pins_changed(c.path.read_text(), new_text, moves, today)
        except RegistryError as e:
            notes.append(f"{c.slug}: {e}; skipped")
            continue
        pid = hashlib.sha256("".join(m.new for m in moves).encode()).hexdigest()[:8]
        proposals.append(
            Proposal(
                slug=c.slug,
                path=c.path.name,
                moves=moves,
                new_text=new_text,
                pin_date=today,
                proposal_id=pid,
                branch=f"{BRANCH_PREFIX}{c.slug}-{pid}",
                title=title_for(c, moves),
                body=render_body(c, moves, today),
            )
        )
    return proposals, notes


def _assert_only_pins_changed(old: str, new: str, moves: list[Move], today: str) -> None:
    """Belt and braces: parse both texts and require that the ONLY differences are
    the moved shas and `pin_date`."""
    a, b = json.loads(old), json.loads(new)
    expect = {m.repo: m.new for m in moves}
    for ra, rb in zip(a["repos"], b["repos"]):
        want = expect.get(ra["repo"], ra["sha"])
        if rb["sha"] != want:
            raise RegistryError(f"rewrite produced the wrong sha for {ra['repo']}")
        ra2, rb2 = dict(ra, sha=""), dict(rb, sha="")
        if ra2 != rb2:
            raise RegistryError(f"rewrite changed more than the sha of {ra['repo']}")
    if len(a["repos"]) != len(b["repos"]):
        raise RegistryError("rewrite changed the number of repos")
    a.pop("repos"), b.pop("repos")
    b_pin = b.pop("pin_date", None)
    a.pop("pin_date", None)
    if b_pin != today or a != b:
        raise RegistryError("rewrite changed more than the pins and pin_date")


# ── repin: opening the pull requests ────────────────────────────────────────


def _run(cmd: list[str], cwd: Optional[pathlib.Path] = None, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, check=check)


def _gh_json(gh: str, args: list[str], cwd: pathlib.Path):
    out = _run([gh, *args], cwd=cwd)
    return json.loads(out.stdout or "null")


def _open_pr(
    p: Proposal, repo_root: pathlib.Path, rel: str, base: str, gh: str, remote: str
) -> str:
    """Branch from the base, commit the one-file change, push, open the PR."""
    _run(["git", "checkout", "-q", "-B", p.branch, f"{remote}/{base}"], cwd=repo_root)
    (repo_root / rel).write_text(p.new_text)
    _run(["git", "add", rel], cwd=repo_root)
    _run(
        [
            "git",
            "-c",
            f"user.name={BOT_NAME}",
            "-c",
            f"user.email={BOT_EMAIL}",
            "commit",
            "-q",
            "-m",
            p.title,
            "-m",
            "Opened by the scheduled hub packs re-pin job; see the pull request.",
        ],
        cwd=repo_root,
    )
    # The branch name embeds the proposal id, so it is ours alone: force is safe.
    _run(["git", "push", "-q", "--force", remote, p.branch], cwd=repo_root)
    return _run(
        [gh, "pr", "create", "--base", base, "--head", p.branch, "--title", p.title, "--body", p.body],
        cwd=repo_root,
    ).stdout.strip()


def apply_proposals(
    proposals: list[Proposal],
    repo_root: pathlib.Path,
    registry_rel: str,
    base: str,
    max_prs: int,
    gh: str = "gh",
    remote: str = "origin",
) -> tuple[list[str], int]:
    """Open one PR per proposal, at most `max_prs` NEW ones.  Idempotent: a
    proposal whose branch already has a PR (open, merged or closed) is left alone,
    so a declined re-pin is not re-opened every week, and skipping it does not use
    up the cap.  Opening a PR for a newer proposal closes the older open ones for
    the same corpus as superseded.  One failing proposal does not stop the rest.
    Returns (one log line per decision, number of failures)."""
    log: list[str] = []
    failures = 0
    opened = 0
    for p in proposals:
        try:
            existing = _gh_json(
                gh, ["pr", "list", "--head", p.branch, "--state", "all", "--json", "number,state"], repo_root
            )
            if existing:
                log.append(
                    f"{p.slug}: already proposed as PR #{existing[0]['number']} "
                    f"({existing[0]['state']}); skipped"
                )
                continue
            if opened >= max_prs:
                log.append(f"{p.slug}: over the {max_prs}-PR cap for this run; the next run continues")
                continue
            url = _open_pr(p, repo_root, f"{registry_rel}/{p.path}", base, gh, remote)
            opened += 1
            log.append(f"{p.slug}: opened {url}")

            open_prs = _gh_json(
                gh, ["pr", "list", "--state", "open", "--limit", "200", "--json", "number,headRefName"], repo_root
            ) or []
            mine = f"{BRANCH_PREFIX}{p.slug}-"
            for other in open_prs:
                head = other["headRefName"]
                if head.startswith(mine) and head != p.branch:
                    _run(
                        [gh, "pr", "close", str(other["number"]), "--delete-branch", "--comment", f"Superseded by {url}"],
                        cwd=repo_root,
                        check=False,
                    )
                    log.append(f"{p.slug}: closed superseded PR #{other['number']}")
        except subprocess.CalledProcessError as e:
            failures += 1
            detail = " ".join((e.stderr or "").split())[:300]
            log.append(f"{p.slug}: FAILED ({' '.join(map(str, e.cmd[:3]))}: {detail})")
    # Leave the checkout where it started, whatever happened above.
    _run(["git", "checkout", "-q", "--detach", f"{remote}/{base}"], cwd=repo_root, check=False)
    return log, failures


# ── catalogue: which packs to download, and whether the result is sane ──────

CATALOGUE_FORMAT_VERSION = 1
CATALOGUE_MAX_BYTES = 2 * 1024 * 1024  # plan §4: the real registry's catalogue is under 2 MB


def fetch_list(corpora: list[Corpus], published_tags: Iterable[str]) -> list[tuple[str, str, str]]:
    """`(slug, pin8, release tag)` for every live corpus whose CURRENT pin has a
    published pack.  A corpus without one (not built yet, or the build failed) is
    simply absent: the catalogue lists corpora that have a pack."""
    tags = list(published_tags)
    out = []
    for c in corpora:
        tag = released_tag(tags, c.slug, c.pin8)
        if tag:
            out.append((c.slug, c.pin8, tag))
    return out


def check_catalogue(
    raw: bytes,
    corpora: list[Corpus],
    repo: str,
    previous: Optional[bytes] = None,
) -> list[str]:
    """Everything that must hold before a freshly built catalogue replaces the live
    one; the list of violations (empty = fine).

    Beyond shape, two guards the signature cannot give: every entry's pin must be
    the manifest's pin and its URL must point at that pin's pack release, and —
    when the live catalogue is given — a corpus that is listed there and still live
    must not vanish.  A catalogue is a complete snapshot, so a half-failed build
    would otherwise silently delist corpora; a stale catalogue is the lesser harm."""
    errors: list[str] = []
    if len(raw) >= CATALOGUE_MAX_BYTES:
        errors.append(f"the catalogue is {len(raw)} bytes, over the {CATALOGUE_MAX_BYTES}-byte budget")
    try:
        cat = json.loads(raw)
    except json.JSONDecodeError as e:
        return [*errors, f"not valid JSON: {e}"]
    if cat.get("format_version") != CATALOGUE_FORMAT_VERSION:
        errors.append(f"format_version is {cat.get('format_version')!r}, expected {CATALOGUE_FORMAT_VERSION}")
    entries = cat.get("corpora")
    if not isinstance(entries, list) or not entries:
        return [*errors, "no corpora"]

    live = {c.slug: c for c in corpora}
    listed: set[str] = set()
    for e in entries:
        slug = e.get("slug", "")
        c = live.get(slug)
        if c is None:
            errors.append(f"{slug or '?'}: not a live lane-A corpus in the registry")
            continue
        if slug in listed:
            errors.append(f"{slug}: listed twice")
        listed.add(slug)
        if e.get("pin_sha") != c.pin_sha:
            errors.append(f"{slug}: pin_sha {str(e.get('pin_sha'))[:12]} is not the manifest's {c.pin8}")
        url = e.get("pack_url", "")
        want = (
            r"^https://github\.com/" + re.escape(repo) + r"/releases/download/"
            + re.escape(pack_tag(slug, c.pin8)) + r"(?:-r\d+)?/"
            + re.escape(f"{slug}-{c.pin8}.hubpack.tar.zst") + "$"
        )
        if not re.match(want, url):
            errors.append(f"{slug}: pack_url {url!r} is not the release asset of pin {c.pin8}")
        if e.get("sig_url") != url + ".sig":
            errors.append(f"{slug}: sig_url is not pack_url + '.sig'")
        if not re.fullmatch(r"[0-9a-f]{64}", str(e.get("pack_sha256", ""))):
            errors.append(f"{slug}: pack_sha256 is not 64 hex")
        size = e.get("pack_bytes")
        if not isinstance(size, int) or isinstance(size, bool) or size <= 0:
            errors.append(f"{slug}: pack_bytes is not a positive integer")

    if previous is not None:
        # The guard must never switch itself off: a "previous catalogue" that is
        # not one (a static site's HTML page answering with 200, say) is an error,
        # not an empty list of corpora.
        before_list = None
        try:
            prev = json.loads(previous)
            if isinstance(prev, dict) and isinstance(prev.get("corpora"), list):
                before_list = prev["corpora"]
        except (json.JSONDecodeError, UnicodeDecodeError):
            pass
        if before_list is None or not all(isinstance(x, dict) for x in before_list):
            return [*errors, "the previous (live) catalogue is not a catalogue; cannot tell which corpora it lists"]
        before = {x.get("slug") for x in before_list}
        for slug in sorted((before & set(live)) - listed):
            errors.append(f"{slug}: in the live catalogue and still live, but missing from the new one")
    return errors


# ── backfill report: one row per corpus per run (spec §12.1) ────────────────
#
# Every pack job records what happened to its corpus (`result-row`), success or
# not; the `report` job folds those rows into benchmarks/hub-backfill/ on
# corpus-hub. A corpus the plan scheduled but which left no row (its job never
# started, was cancelled before recording, or the binary build failed) still gets
# a row saying so: a failed corpus must never vanish silently.

REPORT_ERROR_MAX = 240
_ANSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
_ERRORISH_RE = re.compile(r"error|fail|panic|refus|cannot|denied|timed? ?out", re.I)
# "0 errors", "failed=0", "no failures": error words that report success.
_NOT_ERROR_RE = re.compile(r"\b(?:0|no|zero) (?:errors?|failures?)\b|\b(?:errors?|failed|failures?)[=:] ?0\b", re.I)
# The report is committed: GitHub masks secrets in its own log, not in these lines.
_SECRETISH_RE = re.compile(r"gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|\b[0-9a-fA-F]{64}\b")
REPORT_RUNS_SHOWN = 30


def _log_tail_error(paths: Iterable[pathlib.Path]) -> str:
    """The most telling line of a failed stage's log: the last `::error` annotation,
    else the last line that looks like an error, else the last non-empty line."""
    lines: list[tuple[bool, str]] = []
    for p in paths:
        try:
            text = p.read_text(errors="replace")
        except OSError:
            continue
        for ln in text.splitlines():
            ln = _ANSI_RE.sub("", ln).strip()
            annotated = ln.startswith("::error")
            ln = re.sub(r"^::(?:error|warning|notice)[^:]*::", "", ln).strip()
            if ln:
                lines.append((annotated, ln))
    if not lines:
        return ""
    def errorish(ln: str) -> bool:
        return bool(_ERRORISH_RE.search(ln)) and not _NOT_ERROR_RE.search(ln)

    chosen = lines[-1][1]
    for pick in (lambda a, ln: a, lambda a, ln: errorish(ln)):
        hit = next((ln for a, ln in reversed(lines) if pick(a, ln)), None)
        if hit is not None:
            chosen = hit
            break
    return _SECRETISH_RE.sub("[redacted]", chosen)


def result_row(
    slug: str,
    pin8: str,
    steps: dict,
    stages: list[tuple[str, list[pathlib.Path]]],
    build_out: Optional[pathlib.Path],
    published_tag: str,
    dry_run: bool,
    run_id: str,
    date: str,
    job_status: str = "",
) -> dict:
    """What happened to one corpus in one run.  `steps` is the job's
    `toJSON(steps)`; `stages` names the step ids in order with the log files that
    explain each one's failure."""
    row = {
        "run": str(run_id), "date": date, "slug": slug, "pin8": pin8,
        "bytes": None, "defs": None, "result": "", "tag": "", "stage": "", "error": "",
    }
    if build_out is not None:
        try:
            for ln in reversed(build_out.read_text(errors="replace").splitlines()):
                f = ln.split()
                if len(f) == 6 and f[0] == "pack" and f[1] == slug:
                    row["bytes"], row["defs"] = int(f[3]), int(f[5])
                    break
        except (OSError, ValueError):
            pass
    for sid, logs in stages:
        outcome = (steps.get(sid) or {}).get("outcome")
        if outcome in ("failure", "cancelled"):
            row["result"] = "failed" if outcome == "failure" else "cancelled"
            row["stage"] = sid
            row["error"] = _log_tail_error(logs)[:REPORT_ERROR_MAX]
            return row
    ran = [sid for sid, _ in stages if (steps.get(sid) or {}).get("outcome") == "success"]
    if not ran and job_status and job_status != "success":
        # Nothing tracked ran: the job failed or was cancelled before the first stage.
        row["result"] = "failed" if job_status == "failure" else "cancelled"
        row["stage"] = "before start"
        row["error"] = f"the job was {job_status} before its first tracked stage"
        return row
    if dry_run:
        row["result"] = "dry-run ok" if ran else "no result"
    elif published_tag:
        row["result"], row["tag"] = "published", published_tag
    else:
        # Every recorded stage passed or was skipped, yet nothing was published.
        row["result"] = "no result"
        row["error"] = "no stage failed, but no release was published"
    return row


def missing_rows(planned: list[dict], rows: list[dict], run_id: str, date: str, why: str) -> list[dict]:
    """A row for every planned corpus that recorded nothing."""
    seen = {r.get("slug") for r in rows}
    return [
        {
            "run": str(run_id), "date": date, "slug": m["slug"], "pin8": m["pin8"],
            "bytes": None, "defs": None, "result": "no result", "tag": "", "stage": "",
            "error": why,
        }
        for m in planned
        if m["slug"] not in seen
    ]


def _cell(v) -> str:
    if v is None or v == "":
        return ""
    s = re.sub(r"\s+", " ", str(v)).strip()
    return s.replace("\\", "\\\\").replace("|", "\\|")


# Results that count as "this corpus is fine": a published pack (hub-packs.yml), or a
# whole-hub end-to-end pass (whole_hub_e2e.sh, spec §12.4; its rows carry kind "e2e").
OK_RESULTS = ("published", "e2e passed")


def _is_e2e(r: dict) -> bool:
    return r.get("kind") == "e2e"


def _row_table(rows: list[dict]) -> list[str]:
    out = ["| slug | pin8 | bytes | defs | result | error |", "|---|---|---:|---:|---|---|"]
    for r in sorted(rows, key=lambda r: (r.get("result") in OK_RESULTS, r.get("slug", ""))):
        result = r.get("result", "")
        if r.get("tag"):
            result += f" `{r['tag']}`"
        if r.get("stage"):
            result += f" at `{r['stage']}`"
        out.append(
            f"| `{_cell(r.get('slug'))}` | `{_cell(r.get('pin8'))}` | {_cell(r.get('bytes'))} "
            f"| {_cell(r.get('defs'))} | {_cell(result)} | {_cell(r.get('error') or r.get('detail'))} |"
        )
    return out


def render_report(history: list[dict], run_urls: Optional[dict] = None) -> str:
    """REPORT.md from every row so far: the corpora whose latest attempt did not
    publish first, then each run's rows, newest run first. Whole-hub end-to-end
    rows (kind "e2e") are kept apart: a passing e2e is not a publish, and an e2e
    failure must not hide behind an older publish (or the reverse)."""
    run_urls = run_urls or {}
    e2e = [r for r in history if _is_e2e(r)]
    history = [r for r in history if not _is_e2e(r)]
    latest: dict[str, dict] = {}
    runs: dict[str, list[dict]] = {}
    order: list[str] = []
    for r in history:
        latest[r["slug"]] = r
        if r["run"] not in runs:
            runs[r["run"]] = []
            order.append(r["run"])
        runs[r["run"]].append(r)
    bad = [r for r in latest.values() if r.get("result") != "published"]
    ok = len(latest) - len(bad)
    out = [
        "# Hub backfill report",
        "",
        "Written by `.github/workflows/hub-packs.yml` (the `backfill report` job) after every",
        "publishing run; do not edit by hand. One row per corpus per run; the rows behind it",
        "are in `rows.jsonl`. Spec: docs/CORPUS_HUB_V2_IMPLEMENTATION.md §12.1.",
        "",
        "## Current state",
        "",
        f"{len(latest)} corpora attempted: {ok} published at their latest attempt, {len(bad)} not.",
        "",
    ]
    if bad:
        out += ["### Latest attempt did not publish (fix or delist on purpose)", ""]
        out += _row_table(bad)
        out.append("")
    shown = order[-REPORT_RUNS_SHOWN:]
    out += ["## Runs (newest first)", ""]
    if len(shown) < len(order):
        out += [f"The {len(shown)} newest of {len(order)} runs; every row is in `rows.jsonl`.", ""]
    for run in reversed(shown):
        rows = runs[run]
        date = rows[0].get("date", "")
        url = run_urls.get(run) or rows[0].get("url", "")
        title = f"### Run [{run}]({url})" if url else f"### Run {run}"
        n_ok = sum(1 for r in rows if r.get("result") == "published")
        out += [f"{title}, {date}: {n_ok} of {len(rows)} published", ""]
        out += _row_table(rows)
        out.append("")
    if e2e:
        out += _render_e2e(e2e, run_urls)
    return "\n".join(out)


def _render_e2e(rows: list[dict], run_urls: dict) -> list[str]:
    """The newest whole-hub end-to-end run in full, older ones as one line each."""
    runs: dict[str, list[dict]] = {}
    for r in rows:
        runs.setdefault(r["run"], []).append(r)
    order = list(runs)
    newest = runs[order[-1]]
    out = [
        "## Whole-hub end-to-end (spec §12.4)",
        "",
        "Written by `tools/hub-packs/whole_hub_e2e.sh` (`.github/workflows/hub-e2e.yml`): for every",
        "catalogued corpus, `xerj hub pull` + one `--def` + one question from an empty HOME. The",
        "release gate is 100% of catalogued corpora.",
        "",
    ]
    for run in reversed(order[-REPORT_RUNS_SHOWN:]):
        rs = runs[run]
        n_ok = sum(1 for r in rs if r.get("result") in OK_RESULTS)
        url = run_urls.get(run) or rs[0].get("url", "")
        title = f"[{run}]({url})" if url else run
        line = f"{title}, {rs[0].get('date', '')}: {n_ok} of {len(rs)} corpora passed"
        if rs is newest:
            out += [f"### Run {line}", ""] + _row_table(rs) + [""]
            if len(order) > 1:
                out += ["### Earlier e2e runs", ""]
        else:
            out.append(f"- {line}")
    out.append("")
    return out


def read_rows(path: pathlib.Path) -> list[dict]:
    if not path.is_file():
        return []
    return [json.loads(ln) for ln in path.read_text().splitlines() if ln.strip()]


# ── command line ────────────────────────────────────────────────────────────


def _today() -> str:
    return datetime.now(timezone.utc).date().isoformat()


def _split(value: str) -> list[str]:
    return [s for s in re.split(r"[,\s]+", value or "") if s]


def cmd_plan(a: argparse.Namespace) -> int:
    corpora, warnings = load_live(pathlib.Path(a.registry), pathlib.Path(a.backlog))
    for w in warnings:
        print(f"::warning::{w}", file=sys.stderr)
    tags = pathlib.Path(a.published_tags).read_text().split() if a.published_tags else []
    slugs = set(_split(a.slugs)) if a.slugs and a.slugs != "all" else None
    if slugs:
        unknown = slugs - {c.slug for c in corpora}
        for s in sorted(unknown):
            print(f"::warning::{s}: not a live lane-A corpus with a manifest; ignored", file=sys.stderr)
    result = plan(corpora, tags, slugs, a.force, a.max_builds)
    print(
        f"plan: {result['due']} due, {result['count']} scheduled"
        + (" (capped; the next run continues)" if result["capped"] else ""),
        file=sys.stderr,
    )
    print(json.dumps(result, separators=(",", ":")))
    return 0


def cmd_fetch_list(a: argparse.Namespace) -> int:
    corpora, warnings = load_live(pathlib.Path(a.registry), pathlib.Path(a.backlog))
    for w in warnings:
        print(f"::warning::{w}", file=sys.stderr)
    tags = pathlib.Path(a.published_tags).read_text().split()
    rows = fetch_list(corpora, tags)
    print(f"fetch-list: {len(rows)} of {len(corpora)} live corpora have a published pack", file=sys.stderr)
    for slug, pin8, tag in rows:
        print(f"{slug}\t{pin8}\t{tag}")
    return 0


def cmd_check_catalogue(a: argparse.Namespace) -> int:
    corpora, _ = load_live(pathlib.Path(a.registry), pathlib.Path(a.backlog))
    raw = pathlib.Path(a.catalogue).read_bytes()
    previous = pathlib.Path(a.previous).read_bytes() if a.previous else None
    errors = check_catalogue(raw, corpora, a.repo, previous)
    if a.allow_drops:
        errors = [e for e in errors if "missing from the new one" not in e]
    for e in errors:
        print(f"::error title=catalogue::{e}", file=sys.stderr)
    if errors:
        print(f"check-catalogue: {len(errors)} problem(s); the catalogue is NOT fit to publish", file=sys.stderr)
        return 1
    n = len(json.loads(raw)["corpora"])
    print(f"check-catalogue: {n} corpora, {len(raw)} bytes, consistent with the registry", file=sys.stderr)
    return 0


def cmd_repin(a: argparse.Namespace) -> int:
    registry = pathlib.Path(a.registry)
    corpora, warnings = load_live(registry, pathlib.Path(a.backlog))
    for w in warnings:
        print(f"::warning::{w}", file=sys.stderr)
    classes = _split(a.classes) or list(REPIN_CLASSES)
    if a.include_pinned and "pinned" not in classes:
        classes.append("pinned")
    allow = "file:https" if a.allow_file_urls else "https"
    proposals, notes = find_proposals(
        corpora, classes, lambda u: git_head_of(u, allow), a.today or _today(),
        set(_split(a.slugs)) if a.slugs else None,
    )
    for n in notes:
        print(f"::warning::{n}", file=sys.stderr)
    print(f"repin: classes={','.join(classes)}; {len(proposals)} corpus(es) moved", file=sys.stderr)
    if a.out:
        pathlib.Path(a.out).write_text(json.dumps([p.to_json() for p in proposals], indent=1))
    if not a.apply:
        for p in proposals:
            print(f"would open: {p.title}  [{p.branch}]")
        return 0
    root = pathlib.Path(a.repo_root).resolve()
    log, failures = apply_proposals(
        proposals, root, str(registry.resolve().relative_to(root)), a.base, a.max_prs, a.gh
    )
    for line in log:
        print(line)
    if failures:
        print(f"::error::{failures} re-pin PR(s) could not be opened", file=sys.stderr)
    return 1 if failures else 0


def cmd_result_row(a: argparse.Namespace) -> int:
    stages = []
    for spec in a.stage:
        sid, _, files = spec.partition("=")
        stages.append((sid, [pathlib.Path(f) for f in _split(files)]))
    steps = json.loads(a.steps or "{}")
    row = result_row(
        a.slug, a.pin8, steps, stages,
        pathlib.Path(a.build_out) if a.build_out else None,
        a.published_tag, a.dry_run, a.run_id, a.date or _today(), a.job_status,
    )
    print(json.dumps(row, separators=(",", ":")))
    return 0


def cmd_report(a: argparse.Namespace) -> int:
    planned = json.loads(pathlib.Path(a.matrix).read_text()).get("include", []) if a.matrix else []
    date = a.date or _today()
    rows: list[dict] = []
    if a.rows_dir and pathlib.Path(a.rows_dir).is_dir():
        for f in sorted(pathlib.Path(a.rows_dir).glob("*.json")):
            try:
                r = json.loads(f.read_text())
            except json.JSONDecodeError:
                print(f"::warning::{f.name}: unreadable result row", file=sys.stderr)
                continue
            if isinstance(r, dict) and r.get("slug"):
                rows.append(r)
    rows += missing_rows(planned, rows, a.run_id, date, a.missing_reason)
    for r in rows:
        r["run"] = str(a.run_id)
        r.setdefault("date", date)
        if a.run_url:
            r["url"] = a.run_url
    history_path = pathlib.Path(a.history)
    history = [r for r in read_rows(history_path) if r.get("run") != str(a.run_id)] + rows
    if a.write:
        history_path.parent.mkdir(parents=True, exist_ok=True)
        history_path.write_text("".join(json.dumps(r, separators=(",", ":")) + "\n" for r in history))
        pathlib.Path(a.report).write_text(render_report(history))
    bad = [r for r in rows if r.get("result") not in OK_RESULTS]
    verb = "passed end-to-end" if rows and all(_is_e2e(r) for r in rows) else "published"
    print(f"### backfill report: run {a.run_id}\n")
    print(f"{len(rows) - len(bad)} of {len(rows)} corpora {verb} in this run.\n")
    print("\n".join(_row_table(rows)))
    return 0


def main(argv: Optional[list[str]] = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("plan", help="print the build matrix as JSON")
    p.add_argument("--registry", required=True)
    p.add_argument("--backlog", required=True)
    p.add_argument("--published-tags", help="file with one published release tag per line")
    p.add_argument("--slugs", default="", help="comma list, or 'all' (default: everything due)")
    p.add_argument("--force", action="store_true", help="rebuild pins that already have a release")
    p.add_argument("--max-builds", type=int, default=40)
    p.set_defaults(fn=cmd_plan)

    f = sub.add_parser("fetch-list", help="slug<TAB>pin8<TAB>tag of each live corpus's published pack")
    f.add_argument("--registry", required=True)
    f.add_argument("--backlog", required=True)
    f.add_argument("--published-tags", required=True)
    f.set_defaults(fn=cmd_fetch_list)

    k = sub.add_parser("check-catalogue", help="refuse a catalogue that disagrees with the registry")
    k.add_argument("--registry", required=True)
    k.add_argument("--backlog", required=True)
    k.add_argument("--catalogue", required=True)
    k.add_argument("--previous", help="the live catalogue, to refuse delisting a live corpus")
    k.add_argument("--repo", required=True, help="owner/name of the repository that hosts the releases")
    k.add_argument("--allow-drops", action="store_true", help="accept delisting corpora that are still live")
    k.set_defaults(fn=cmd_check_catalogue)

    r = sub.add_parser("repin", help="propose pin moves for corpora whose upstream moved")
    r.add_argument("--registry", required=True)
    r.add_argument("--backlog", required=True)
    r.add_argument("--classes", default=",".join(REPIN_CLASSES))
    r.add_argument("--include-pinned", action="store_true", help="also re-pin the `pinned` class")
    r.add_argument("--slugs", default="")
    r.add_argument("--today", default="")
    r.add_argument("--out", help="write the proposals as JSON")
    r.add_argument("--apply", action="store_true", help="open the PRs (default: print what would be opened)")
    r.add_argument("--repo-root", default=".")
    r.add_argument("--base", default="corpus-hub")
    r.add_argument("--max-prs", type=int, default=25)
    r.add_argument("--gh", default="gh")
    r.add_argument("--allow-file-urls", action="store_true", help=argparse.SUPPRESS)
    r.set_defaults(fn=cmd_repin)

    rr = sub.add_parser("result-row", help="one corpus's result in this run, as a JSON row")
    rr.add_argument("--slug", required=True)
    rr.add_argument("--pin8", required=True)
    rr.add_argument("--steps", default="{}", help="the job's toJSON(steps)")
    rr.add_argument("--stage", action="append", default=[], help="step id=log,log (in order)")
    rr.add_argument("--build-out", help="build-pack's stdout (its last line carries bytes and defs)")
    rr.add_argument("--published-tag", default="")
    rr.add_argument("--dry-run", action="store_true")
    rr.add_argument("--run-id", required=True)
    rr.add_argument("--date", default="")
    rr.add_argument("--job-status", default="", help="the job's job.status")
    rr.set_defaults(fn=cmd_result_row)

    rp = sub.add_parser("report", help="fold this run's result rows into the backfill report")
    rp.add_argument("--rows-dir", help="directory of result-row JSON files")
    rp.add_argument("--matrix", help="the plan's JSON (its `include` is every scheduled corpus)")
    rp.add_argument("--missing-reason", default="the pack job recorded no result")
    rp.add_argument("--run-id", required=True)
    rp.add_argument("--run-url", default="")
    rp.add_argument("--date", default="")
    rp.add_argument("--history", default="benchmarks/hub-backfill/rows.jsonl")
    rp.add_argument("--report", default="benchmarks/hub-backfill/REPORT.md")
    rp.add_argument("--write", action="store_true", help="update the history and REPORT.md (default: print only)")
    rp.set_defaults(fn=cmd_report)

    a = ap.parse_args(argv)
    return a.fn(a)


if __name__ == "__main__":
    sys.exit(main())
