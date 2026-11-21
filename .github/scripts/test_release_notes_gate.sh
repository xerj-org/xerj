#!/usr/bin/env bash
# test_release_notes_gate.sh - offline fixture tests for release-notes-gate.sh.
#
# Each case builds a throwaway git repo with engine/Cargo.toml, CHANGELOG.md
# and ROADMAP.md, tags v0.1.0 as the previous release, and runs the gate with
# a stub `gh` first on PATH (no network, no token). The stub answers the
# closing-issues GraphQL query with nothing, so an uncited PR stays uncited.
#
# The case that motivated this file: a window with NO commits (HEAD is the
# previous tag itself, e.g. a re-run after the tag was pushed) used to FAIL
# with "found no PR-numbered merges ... the parse broke". Nothing merged means
# nothing can be missing from the notes, so it must PASS.
#
# USAGE
#   bash .github/scripts/test_release_notes_gate.sh
#   GATE=/path/to/other/release-notes-gate.sh bash .github/scripts/test_release_notes_gate.sh

set -uo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
GATE="${GATE:-$HERE/release-notes-gate.sh}"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

mkdir -p "$WORK/bin"
cat >"$WORK/bin/gh" <<'EOF'
#!/bin/sh
exit 0
EOF
chmod +x "$WORK/bin/gh"

failures=0
ok()  { printf 'ok    %s\n' "$1"; }
bad() { printf 'FAIL  %s\n' "$1"; failures=$((failures + 1)); }

# fixture <dir> <changelog-body> - repo at v0.1.0, cutting 0.2.0.
fixture() {
  local d="$1"
  mkdir -p "$d/engine"
  git -C "$d" init -q
  git -C "$d" config user.email fixture@example.invalid
  git -C "$d" config user.name fixture
  git -C "$d" config commit.gpgsign false
  git -C "$d" config tag.gpgsign false
  printf '[workspace.package]\nversion = "0.2.0"\n' >"$d/engine/Cargo.toml"
  printf '# Roadmap\n\n**Open defects** (shortlist)\n\nNone.\n\n## Next\n' >"$d/ROADMAP.md"
  printf '# Changelog\n\n## [0.2.0]\n\n%s\n\n## [0.1.0]\n\n- first\n' "$2" >"$d/CHANGELOG.md"
  git -C "$d" add -A
  git -C "$d" commit -q -m "release: v0.1.0"
  git -C "$d" tag v0.1.0
}

# run_gate <dir> - runs the gate in <dir>; sets OUT and RC.
run_gate() {
  OUT="$(cd "$1" && PATH="$WORK/bin:$PATH" BASE_REF=v0.1.0 XERJ_REPO=fixture/fixture bash "$GATE" 2>&1)"
  RC=$?
}

expect() { # <name> <want-rc> <want-substring>
  if [ "$RC" -eq "$2" ] && printf '%s\n' "$OUT" | grep -F -- "$3" >/dev/null; then
    ok "$1"
  else
    bad "$1 (rc=$RC, want $2 and \"$3\")"
    printf '%s\n' "$OUT" | sed 's/^/      /'
  fi
}

echo '== case 1: empty window (HEAD is the previous tag) =='
d="$WORK/empty"; fixture "$d" "- nothing yet"
run_gate "$d"
expect "empty window passes" 0 "no commits in v0.1.0..HEAD"
expect "empty window verdict" 0 "PASS  the [0.2.0] notes match the tree"

echo '== case 2: commits but none PR-numbered (parse or window broke) =='
d="$WORK/noprs"; fixture "$d" "- nothing yet"
git -C "$d" commit -q --allow-empty -m "docs: tweak"
run_gate "$d"
expect "non-empty window with no PR merges still fails" 1 "found no PR-numbered merges among the 1 commit(s) in v0.1.0..HEAD"

echo '== case 3: one merged PR, cited in the notes =='
d="$WORK/cited"; fixture "$d" "- fix the thing (#12)"
git -C "$d" commit -q --allow-empty -m "fix: the thing (#12)"
run_gate "$d"
expect "cited PR passes" 0 "PR #12 cited in [0.2.0]"

echo '== case 4: one merged PR, not cited =='
d="$WORK/uncited"; fixture "$d" "- nothing yet"
git -C "$d" commit -q --allow-empty -m "fix: other (#13)"
run_gate "$d"
expect "uncited PR fails" 1 "PR #13 merged in v0.1.0..HEAD but the [0.2.0] notes never mention it"

printf '\n'
if [ "$failures" -eq 0 ]; then
  echo "PASS  release-notes-gate fixtures"
else
  echo "FAIL  $failures release-notes-gate fixture check(s)"
  exit 1
fi
