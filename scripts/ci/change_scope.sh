#!/usr/bin/env bash
# Classify a change set for .github/workflows/ci.yml's `changes` job.
#
# Reads changed file paths (repo-relative, one per line) on stdin and prints:
#   code=true|false   false ONLY when every path is on the docs-only allowlist
#   ux=true|false     true when anything under xerj-ux/ changed
#
# It FAILS OPEN: empty input, an unknown path, or any doubt means code=true,
# i.e. run every job. Skipping a job wrongly costs a red main later; running
# it needlessly only costs runner minutes. Tests: scripts/ci/test_change_scope.sh
#
# The allowlist is deliberately narrow. Most of this repo's prose is NOT inert:
# Build + Test carries doc-claim tests that read named docs surfaces and assert
# on their contents (engine/crates/xerj-engine/tests/docs_capability_lists.rs,
# docs_do_not_promise_the_next_release.rs, security_tooling_claims.rs,
# engine/crates/xerj-rerank/tests/egress_inventory.rs,
# engine/crates/xerj-api/tests/scroll_snapshot_cap_is_documented.rs,
# engine/crates/xerj-common/src/config.rs's include_str! of landing/docs/config.html,
# engine/crates/xerj-mcp/tests/published_schema_drift.rs, ...), and the
# use-case smoke job lints landing/get{,.ps1}. Every file those read BY NAME is
# listed in is_test_input below and always counts as code. Add to that list
# whenever a test starts reading a new docs file.
set -u

is_test_input() {
  case "$1" in
    # Root/engine prose that doc-claim tests read by name.
    README.md|ROADMAP.md|SECURITY.md|CHANGELOG.md|*/CHANGELOG.md) return 0 ;;
    docs/RERANK.md|docs/ZERO_TOKEN_DIRECTION.md|docs/ARCHITECTURE.md) return 0 ;;
    docs/recipes/air-gapped-deployment.md|docs/media/*) return 0 ;;
    user-feedback/09-security/cves-and-vulnerabilities.md) return 0 ;;
    # Published surfaces with generated/asserted sections.
    landing/llms.txt|landing/llms-full.txt|landing/openapi.json) return 0 ;;
    landing/docs/agents/schemas/*) return 0 ;;
    landing/docs/queries.html|landing/docs/aggregations.html) return 0 ;;
    landing/docs/api-es-compat.html|landing/docs/migration-from-es.html) return 0 ;;
    landing/docs/config.html|landing/docs/env.html) return 0 ;;
    landing/docs/recipes/air-gapped-deployment.html) return 0 ;;
    landing/answers/rerank-search-results-calibrated-judge.*) return 0 ;;
    landing/pricing/index.html|landing/demo/index.html) return 0 ;;
    # Installer scripts: linted + exercised by the use-case smoke job.
    landing/get|landing/get.ps1) return 0 ;;
  esac
  return 1
}

is_docs_only() {
  local p="$1"
  is_test_input "$p" && return 1
  case "$p" in
    # Never docs, whatever the extension: code trees, test fixtures, the
    # reference-coding toolkit, use-case harnesses, the console (embedded into
    # the server binary by xerj-console-api/build.rs), SEO sources.
    engine/*|xerj-ux/*|tools/*|demo/usecases/*|content/*|scripts/*|.github/*) return 1 ;;
    */fixtures/*|*/testdata/*|*/test/*|*/tests/*) return 1 ;;
  esac
  case "$p" in
    docs/*|user-feedback/*|landing/*) return 0 ;;
    .contributors|.gitignore|LICENSE*|README*) return 0 ;;
    benchmarks/*.md) return 0 ;;
    *.md) return 0 ;;
  esac
  return 1
}

code=false
ux=false
seen=0
while IFS= read -r path || [ -n "$path" ]; do
  path="${path%$'\r'}"
  [ -z "$path" ] && continue
  seen=$((seen + 1))
  case "$path" in xerj-ux/*) ux=true ;; esac
  if ! is_docs_only "$path"; then
    code=true
  fi
done

# Nothing to classify (empty diff, failed diff upstream): fail open.
[ "$seen" -eq 0 ] && code=true

echo "code=$code"
echo "ux=$ux"
