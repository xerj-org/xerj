#!/usr/bin/env bash
set -euo pipefail

repo_root=$(CDPATH= cd -- "$(dirname -- "$0")/../../.." && pwd)
plugin_root="$repo_root/integrations/herdr"
workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT
workspace="$workdir/worktree"
mkdir -p "$workspace"
hook="$plugin_root/scripts/on-worktree-created"
indexer="$plugin_root/scripts/index-workspace"

mock_xerj="$workdir/mock-xerj"
cat > "$mock_xerj" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$PWD" > "$MOCK_CWD"
printf '%s\n' "$*" > "$MOCK_ARGS"
case "${1:-}" in
  init)
    printf '{"mcpServers":{"xerj":{"command":"xerj","args":["mcp"]}}}\n' > .mcp.json
    ;;
  autoindex)
    ;;
  *)
    exit 64
    ;;
esac
MOCK
chmod +x "$mock_xerj"

payload=$(python3 - "$workspace" <<'PY'
import json
import sys

path = sys.argv[1]
print(json.dumps({
    "event": "worktree_created",
    "data": {
        "type": "worktree_created",
        "workspace": {
            "workspace_id": "w-test",
            "number": 1,
            "label": "herdr-test",
            "focused": False,
            "pane_count": 1,
            "tab_count": 1,
            "active_tab_id": "w-test:t1",
            "agent_status": "idle",
            "worktree": {
                "repo_key": "test-repo",
                "repo_name": "xerj",
                "repo_root": path,
                "checkout_path": path,
                "is_linked_worktree": True,
            },
        },
        "worktree": {
            "path": path,
            "branch": "herdr-test",
            "is_bare": False,
            "is_detached": False,
            "is_prunable": False,
            "is_linked_worktree": True,
            "open_workspace_id": "w-test",
            "label": "herdr-test",
        },
    },
}))
PY
)

MOCK_CWD="$workdir/init-cwd" MOCK_ARGS="$workdir/init-args" XERJ_BIN="$mock_xerj" \
  HERDR_PLUGIN_EVENT=worktree.created HERDR_PLUGIN_EVENT_JSON="$payload" \
  "$hook" >/dev/null

test "$(cat "$workdir/init-cwd")" = "$workspace"
test "$(cat "$workdir/init-args")" = "init"
test -s "$workspace/.mcp.json"

expect_hook_rejection() {
  local event_json=$1
  local expected_status=$2
  if HERDR_PLUGIN_EVENT=worktree.created HERDR_PLUGIN_EVENT_JSON="$event_json" \
    XERJ_BIN="$mock_xerj" "$hook" >/dev/null 2>/dev/null; then
    printf 'expected hook rejection, but it succeeded\n' >&2
    return 1
  else
    test "$?" -eq "$expected_status"
  fi
}

expect_hook_rejection '{not-json' 65
expect_hook_rejection '{"event":"worktree_created","data":{"type":"worktree_created","workspace":{},"worktree":{}}}' 65
mismatched_payload=$(python3 - "$workspace" "$workdir/other-worktree" <<'PY'
import json
import sys

print(json.dumps({
    "event": "worktree_created",
    "data": {
        "type": "worktree_created",
        "workspace": {"worktree": {"checkout_path": sys.argv[1]}},
        "worktree": {"path": sys.argv[2]},
    },
}))
PY
)
expect_hook_rejection "$mismatched_payload" 65

MOCK_CWD="$workdir/index-cwd" MOCK_ARGS="$workdir/index-args" XERJ_BIN="$mock_xerj" \
HERDR_PLUGIN_CONTEXT_JSON="$(python3 - "$workspace" <<'PY'
import json
import sys
print(json.dumps({"workspace_id": "w-test", "focused_pane_cwd": sys.argv[1]}))
PY
)" \
  "$indexer" >/dev/null

test "$(cat "$workdir/index-cwd")" = "$workspace"
test "$(cat "$workdir/index-args")" = "autoindex ."

printf 'PASS: worktree.created runs xerj init and creates .mcp.json\n'
printf 'PASS: pane action runs xerj autoindex . in the focused workspace\n'
