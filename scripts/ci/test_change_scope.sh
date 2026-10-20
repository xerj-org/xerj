#!/usr/bin/env bash
# Tests for scripts/ci/change_scope.sh. Run by the `changes` job in ci.yml
# before the classifier is trusted, and runnable locally:
#   bash scripts/ci/test_change_scope.sh
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
SCOPE="$HERE/change_scope.sh"
fails=0

check() { # name expected-code expected-ux files...
  local name="$1" want_code="$2" want_ux="$3"
  shift 3
  local out
  out="$(printf '%s\n' "$@" | bash "$SCOPE")"
  local want
  want="$(printf 'code=%s\nux=%s' "$want_code" "$want_ux")"
  if [ "$out" = "$want" ]; then
    echo "ok   - $name"
  else
    echo "FAIL - $name: want [$(echo $want)] got [$(echo $out)]"
    fails=$((fails + 1))
  fi
}

check "docs-only"            false false docs/WHY_XERJ.md landing/blog/index.html user-feedback/x/report.md .contributors LICENSE benchmarks/idle-budget/README.md notes/thing.md
check "mixed docs + rust"    true  false docs/WHY_XERJ.md engine/crates/xerj-engine/src/index.rs
check "rust-only"            true  false engine/crates/xerj-engine/src/index.rs
check "xerj-ux-only"         true  true  xerj-ux/console.js
check "CHANGELOG"            true  false CHANGELOG.md
check "tools/xerj-code"      true  false tools/xerj-code/SKILL.md
check "workflow"             true  false .github/workflows/ci.yml
check "test-read doc"        true  false docs/RERANK.md
check "test-read landing"    true  false landing/llms.txt
check "root README"          true  false README.md
check "installer"            true  false landing/get
check "test fixture .md"     true  false engine/crates/xerj-autoindex/tests/fixtures/a.md

# Empty input (an empty or failed diff) must fail open.
out="$(printf '' | bash "$SCOPE")"
if [ "$out" = "$(printf 'code=true\nux=false')" ]; then
  echo "ok   - empty input"
else
  echo "FAIL - empty input: got [$(echo $out)]"
  fails=$((fails + 1))
fi

if [ "$fails" -ne 0 ]; then
  echo "$fails change_scope test(s) failed"
  exit 1
fi
echo "all change_scope tests passed"
