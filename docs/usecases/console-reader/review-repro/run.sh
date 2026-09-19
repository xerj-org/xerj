#!/usr/bin/env bash
# Replay the correctness review of PR #945 against a REAL xerj node (auth on).
#
#   ./run.sh <path-to-xerj-binary> [port]        # default port 9560 (+1, +2)
#
# Builds the edge-case folder (mkrepro.py), runs `xerj brain` on it — which
# boots the node — then live-repro.mjs: what the engine holds, what the first
# version of the Reader asked for and got, and what the bundled console shows in
# headless Chrome now. Output: review-repro.json. Needs Node >= 22, Chrome (or
# CHROME_BIN), python3. Everything is written under $WORK; the node is stopped
# at the end. Like ../run.sh it refuses a port that is already in use.
set -euo pipefail
XERJ="${1:?usage: run.sh <xerj binary> [port]}"
PORT="${2:-9560}"
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK="${WORK:-$(mktemp -d)}"
mkdir -p "$WORK"
for p in "$PORT" "$((PORT + 1))" "$((PORT + 2))"; do
  if (exec 3<>"/dev/tcp/127.0.0.1/$p") 2>/dev/null; then
    echo "run.sh: port $p is already in use — refusing to run against a node this script did not start" >&2
    exit 2
  fi
done
[ ! -e "$WORK/data/server.pid" ] || { echo "run.sh: $WORK/data/server.pid exists — use a fresh WORK dir" >&2; exit 2; }
python3 "$HERE/mkrepro.py" "$WORK/repro"
trap 'kill "$(cat "$WORK/data/server.pid" 2>/dev/null)" 2>/dev/null || true' EXIT
"$XERJ" brain "$WORK/repro" --brain repro --url "http://localhost:$PORT" \
  --data-dir "$WORK/data" --no-open --disable-feedback 2>&1 | tee "$WORK/brain.log" || true
[ -s "$WORK/data/server.pid" ] && [ -s "$WORK/data/admin.key" ] || { echo "run.sh: xerj brain did not boot a node under $WORK/data" >&2; exit 1; }
BRAIN=repro node "$HERE/live-repro.mjs" "http://localhost:$PORT" "$(cat "$WORK/data/admin.key")" | tee "$WORK/review-repro.json"
