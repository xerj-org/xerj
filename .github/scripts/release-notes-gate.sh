#!/usr/bin/env bash
# release-notes-gate.sh - fail a release PR whose notes assert a state the tree
# does not have (issue #474).
#
# WHY THIS EXISTS
#   The rc.18 release PR (#460) shipped three separate contradictions, each
#   found by human review and none by CI, with the branch 16/16 green every
#   time: a fix credited with more than it did (#361 claimed fixed under
#   "### Fixed" while still open), ROADMAP.md contradicting the CHANGELOG in
#   the same commit (six issues carried as open defects after rc.18 closed
#   them), and a note reverted by a later merge in the same release (#472
#   reverted #457's README change; the notes described the opposite of the
#   shipped README). rc.16 had the identical failure before it. The structural
#   problem: a release PR is cut at time T and describes main at time T; main
#   keeps moving, and nothing re-checked the notes against the tree at merge
#   time. The existing ROADMAP gates in docs_capability_lists.rs compare
#   version strings in headers, so bumping the header is what made them pass.
#   This gate checks the statuses, not the header.
#
# THE THREE CHECKS (from #474, in the issue's order)
#   1. Issue-state. Every issue linked in ROADMAP.md's "Open defects" shortlist
#      must actually be open. Every issue linked under a "### Fixed" heading of
#      the release section being cut must be closed - or the entry must say
#      what remains open (see MARKERS below).
#   2. Coverage, the other direction. Every PR merged between the previous tag
#      and the release head must be cited in the release section, close an
#      issue that is cited there, or be on the explicit exempt list (see
#      EXEMPTIONS). This is what would have caught #472 reverting #457.
#   3. Freshness. Fail if the release head is behind its base - the condition
#      under which every one of the rc.18 drifts happened.
#
# MARKERS - how a "### Fixed" entry may cite an open issue
#   A fix entry may legitimately point at an issue that stays open (a partial
#   fix, or a neighbouring defect it explicitly does not touch), but then the
#   entry has to say so. The entry's text must contain one of:
#     "stays open" / "remains open" / "still open" / "left open" /
#     "not fixed" / "not yet fixed" / "partial" / "unchanged" /
#     "separately tracked" / "tracked in|by" / "follow-up" / "larger change" /
#     "carried forward|into"
#   The rc.18 #361 bullet claimed the fix whole and carried none of these, so
#   it fails; the rc.18 filter-context caveat ("separately-tracked ... this
#   change neither fixes nor worsens it") passes.
#
# EXEMPTIONS - merges the notes deliberately do not cite
#   CI-only and test-only merges may be exempted with an HTML comment INSIDE
#   the release section, so exemptions are release-scoped and reviewed with
#   the notes they exempt from:
#       <!-- notes-exempt: #859 (CI-only) #863 (test-only) -->
#   Any "#N" inside a notes-exempt comment is exempt. Write the reason.
#
# A partially-checked release is not a verified release: an issue or PR whose
# state could not be fetched is counted as a FAILURE, never skipped.
#
# USAGE
#   .github/scripts/release-notes-gate.sh          # gate HEAD (a release branch)
#
#   Replay the rc.18 release PR head (#460) and watch every check fire:
#       git fetch origin pull/460/head
#       git worktree add /tmp/rc18-replay FETCH_HEAD
#       (cd /tmp/rc18-replay && BASE_REF=origin/main \
#           bash path/to/release-notes-gate.sh)   # -> FAIL, exit 1
#   Honest scope of that replay: freshness fails by construction (main moved
#   on), and the issue-state and coverage checks run against TODAY's tracker,
#   not the tracker as it stood at the cut. The three drifts #474 documents
#   were hand-fixed on that head before merge, so what the replay flags is the
#   drift accumulated since - the same classes, caught mechanically this time.
#
# ENV OVERRIDES
#   XERJ_REPO   owner/name for issue-state lookups   (default: xerj-org/xerj)
#   BASE_REF    the ref the release PR merges into   (default: origin/main)
#   GH_TOKEN    token for gh; in CI pass github.token
#
# Requires: gh (authenticated), git history deep enough to reach the previous
# tag (CI checks out with fetch-depth: 0), awk, grep.

set -uo pipefail

REPO="${XERJ_REPO:-xerj-org/xerj}"
BASE_REF="${BASE_REF:-origin/main}"
OWNER="${REPO%%/*}"
NAME="${REPO##*/}"

fails=0
note() { printf '  %s\n' "$1"; }
fail() { printf 'FAIL  %s\n' "$1"; fails=$((fails + 1)); }
pass() { printf 'ok    %s\n' "$1"; }

# Issue state via the REST API. Prints "open" / "closed"; anything else (API
# error, rate limit, nonexistent issue) prints "error" - callers must fail on
# it, not skip it.
issue_state() {
  gh api "repos/$REPO/issues/$1" --jq .state 2>/dev/null || printf 'error'
}

# ── Check 3 first (cheapest): freshness ──────────────────────────────────────
printf '\n== freshness: release head is not behind %s ==\n' "$BASE_REF"
if ! git rev-parse --verify --quiet "$BASE_REF" >/dev/null; then
  fail "base ref $BASE_REF does not resolve - fetch it first (git fetch origin main)"
elif git merge-base --is-ancestor "$BASE_REF" HEAD; then
  pass "HEAD contains $BASE_REF ($(git rev-parse --short "$BASE_REF"))"
else
  behind=$(git rev-list --count HEAD.."$BASE_REF")
  fail "release head is $behind commit(s) behind $BASE_REF - the notes describe a base that has moved (#474 drift condition); merge $BASE_REF and re-review the notes"
fi

# ── The release being cut ────────────────────────────────────────────────────
VERSION="$(awk -F'"' '/^version = /{print $2; exit}' engine/Cargo.toml)"
if [ -z "$VERSION" ]; then
  fail "could not read the version from engine/Cargo.toml - nothing to gate against"
  printf '\n%d failure(s)\n' "$fails"; exit 1
fi
printf '\nrelease being cut: %s (engine/Cargo.toml)\n' "$VERSION"

# The release section: from "## [VERSION]" to the next "## [" heading. The
# release PR is the commit that turns "## [Unreleased]" into this heading, so
# on a release branch it must exist.
SECTION="$(awk -v v="$VERSION" '
  index($0, "## [" v "]") == 1 { on = 1; next }
  on && /^## \[/ { exit }
  on { print }
' CHANGELOG.md)"
if [ -z "$SECTION" ]; then
  fail "CHANGELOG.md has no \"## [$VERSION]\" section - a release PR must cut the [Unreleased] notes over to the version it ships"
fi

# ── Check 1a: ROADMAP open-defect citations are open ─────────────────────────
printf '\n== issue-state: ROADMAP open-defects shortlist ==\n'
SHORTLIST="$(awk '
  /^\*\*Open defects/ { on = 1 }
  on && /^## / { exit }
  on { print }
' ROADMAP.md)"
roadmap_issues="$(printf '%s\n' "$SHORTLIST" | grep -oE 'issues/[0-9]+' | cut -d/ -f2 | sort -un)"
if [ -z "$roadmap_issues" ]; then
  fail "found no linked issues in ROADMAP.md's \"**Open defects\" shortlist - the section moved or the parse broke; fix the gate, do not merge around it"
else
  for n in $roadmap_issues; do
    state="$(issue_state "$n")"
    case "$state" in
      open)   pass "#$n open (ROADMAP lists it as an open defect)" ;;
      closed) fail "#$n is CLOSED but ROADMAP.md still carries it as an open defect - the rc.16/rc.18 drift; remove it or reopen it" ;;
      *)      fail "#$n state could not be fetched (${state}) - unverified is not verified" ;;
    esac
  done
fi

# ── Check 1b: "### Fixed" citations are closed, or the entry says otherwise ──
printf '\n== issue-state: "### Fixed" entries of [%s] ==\n' "$VERSION"
# Entries under every "### Fixed" heading of the section, one per "- " bullet,
# newlines folded so an entry greps as one line.
FIXED_ENTRIES="$(printf '%s\n' "$SECTION" | awk '
  /^### Fixed/ { on = 1; next }
  /^### /      { on = 0 }
  on && /^- /  { if (entry != "") print entry; entry = $0; next }
  on && entry != "" { entry = entry " " $0 }
  END { if (entry != "") print entry }
')"
MARKERS='(stays|remains|still|left) open|not (yet )?fixed|partial|unchanged|separately.tracked|tracked (in|by)|follow.up|larger change|carried (forward|into)'
if [ -z "$FIXED_ENTRIES" ]; then
  note "section [$VERSION] has no \"### Fixed\" entries - nothing to check"
else
  checked=0
  while IFS= read -r entry; do
    for n in $(printf '%s\n' "$entry" | grep -oE 'issues/[0-9]+' | cut -d/ -f2 | sort -un); do
      checked=$((checked + 1))
      state="$(issue_state "$n")"
      case "$state" in
        closed) pass "#$n closed (cited under ### Fixed)" ;;
        open)
          if printf '%s\n' "$entry" | grep -qiE "$MARKERS"; then
            pass "#$n open, and the entry says what remains open"
          else
            fail "#$n is OPEN but its ### Fixed entry claims it whole (the rc.18 #361 drift) - close the issue, or say in the entry what remains open: ${entry:0:100}..."
          fi ;;
        *) fail "#$n state could not be fetched (${state}) - unverified is not verified" ;;
      esac
    done
  done <<EOF
$FIXED_ENTRIES
EOF
  note "$checked issue citation(s) checked under ### Fixed"
fi

# ── Check 2: every merged PR since the previous tag is cited or exempt ───────
printf '\n== coverage: merges since the previous tag are all in the notes ==\n'
PREV_TAG="$(git describe --tags --abbrev=0 --match 'v*' HEAD 2>/dev/null || true)"
if [ -z "$PREV_TAG" ]; then
  fail "no previous v* tag reachable from HEAD - coverage needs full history (checkout with fetch-depth: 0)"
elif [ -n "$SECTION" ]; then
  exempt="$(printf '%s\n' "$SECTION" | grep -oE '<!--[[:space:]]*notes-exempt:[^>]*' | grep -oE '#[0-9]+' | tr -d '#' | sort -un)"
  merged_prs="$(git log --first-parent --format='%s' "$PREV_TAG..HEAD" | awk '
    match($0, /\(#[0-9]+\)$/)          { print substr($0, RSTART + 2, RLENGTH - 3); next }
    match($0, /^Merge pull request #[0-9]+/) { s = substr($0, RSTART + 20, RLENGTH - 20); print s }
  ' | sort -un)"
  if [ -z "$merged_prs" ]; then
    fail "found no PR-numbered merges in $PREV_TAG..HEAD - wrong window or the parse broke; fix the gate, do not merge around it"
  fi
  for n in $merged_prs; do
    if printf '%s\n' "$exempt" | grep -qx "$n"; then
      note "PR #$n on the notes-exempt list"
      continue
    fi
    if printf '%s\n' "$SECTION" | grep -qE "#$n([^0-9]|\$)|/(pull|issues)/$n([^0-9]|\$)"; then
      pass "PR #$n cited in [$VERSION]"
      continue
    fi
    # Last chance: the notes may cite the ISSUE the PR closed instead of the
    # PR itself. Ask the API which issues this PR closes.
    closing="$(gh api graphql \
      -f query='query($o:String!,$r:String!,$n:Int!){repository(owner:$o,name:$r){pullRequest(number:$n){closingIssuesReferences(first:20){nodes{number}}}}}' \
      -f o="$OWNER" -f r="$NAME" -F n="$n" \
      --jq '.data.repository.pullRequest.closingIssuesReferences.nodes[].number' 2>/dev/null)" || closing=""
    cited_via=""
    for i in $closing; do
      if printf '%s\n' "$SECTION" | grep -qE "#$i([^0-9]|\$)|/(pull|issues)/$i([^0-9]|\$)"; then
        cited_via="$i"; break
      fi
    done
    if [ -n "$cited_via" ]; then
      pass "PR #$n cited via the issue it closes (#$cited_via)"
    else
      subject="$(git log --first-parent --format='%s' "$PREV_TAG..HEAD" | grep -F "#$n" | head -1)"
      fail "PR #$n merged in $PREV_TAG..HEAD but the [$VERSION] notes never mention it or an issue it closes (the #472-reverting-#457 drift) - cite it, or exempt it with a reason: ${subject:-<subject not found>}"
    fi
  done
fi

# ── Verdict ──────────────────────────────────────────────────────────────────
printf '\n'
if [ "$fails" -eq 0 ]; then
  printf 'PASS  the [%s] notes match the tree they describe\n' "$VERSION"
else
  printf 'FAIL  %d contradiction(s) between the notes and the tree (#474)\n' "$fails"
  exit 1
fi
