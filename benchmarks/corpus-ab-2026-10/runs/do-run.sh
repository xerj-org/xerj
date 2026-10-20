#!/bin/sh
# One A/B run: fresh clone at the pinned SHA, identical task file, token
# accounting. Usage: do-run.sh <P|X> <id>   (expects runs/SHA.txt, a running
# node on :9831 for X, no node for P, and the release binary at
# /tmp/xtarget-1093/release/xerj)
set -e
ARM=$1; ID=$2
SHA=$(cat "$(dirname "$0")"/SHA.txt)
BIN=/tmp/xtarget-1093/release/xerj
BASE=/root/ab-run-$ID
[ -x "$BIN" ] || { echo "no binary at $BIN" >&2; exit 1; }
rm -rf "$BASE"; mkdir -p "$BASE/out"
git clone -q /workspace "$BASE/repo"
git -C "$BASE/repo" checkout -q "$SHA"
sed "s/<ID>/$ID/g" "$(dirname "$0")"/../TASK-$ARM.md > "$BASE/TASK.md"
cd "$BASE/repo"
if [ "$ARM" = X ]; then export XERJ_URL=http://localhost:9831; else unset XERJ_URL || true; fi
START=$(date +%s)
set +e
claude -p \
  --dangerously-skip-permissions --max-turns 80 --output-format json \
  < "$BASE/TASK.md" \
  > "$BASE/usage.raw.json" 2> "$BASE/claude.stderr"
RC=$?
set -e
END=$(date +%s)
echo "{\"arm\": \"$ARM\", \"id\": \"$ID\", \"sha\": \"$SHA\", \"claude_exit\": $RC, \"wall_s\": $((END-START))}" > "$BASE/runmeta.json"
cp -r "$HOME/.claude/projects/-root-ab-run-$ID-repo" "$BASE/transcript" 2>/dev/null || true
echo "run $ID (arm $ARM) done: exit=$RC wall=$((END-START))s"
ls -la "$BASE/out" || true
