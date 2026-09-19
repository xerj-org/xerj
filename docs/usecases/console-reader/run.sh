#!/usr/bin/env bash
# Live end-to-end run of the console's corpus home, reader and guest mode
# against a REAL xerj node with auth ON (the default).
#
#   ./run.sh <path-to-xerj-binary> [port]        # default port 9550 (+1, +2)
#
# It builds a small folder of email (two with PDF attachments, one hostile in
# every header an outsider controls) plus PDFs and notes, runs
# `xerj brain <folder>` — which boots the node, indexes, detects links and
# prints the one-time passkey setup link — then drives headless Chrome:
#   A. operator: enrol a passkey (virtual authenticator) → Corpus → Reader → Discover
#   B. guest:    mint a read-only key scoped to the email index + the brain,
#                put {api_key,index,brain,expires_at,label} in sessionStorage,
#                open the console.
# Output: live-e2e.json (what it saw) and PNG screenshots in the work dir.
#
# Needs: Node >= 22, Chrome (or CHROME_BIN), python3. Everything it writes goes
# under $WORK; the node is stopped at the end.
set -euo pipefail
XERJ="${1:?usage: run.sh <xerj binary> [port]}"
PORT="${2:-9550}"
HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../../.." && pwd)"
WORK="${WORK:-$(mktemp -d)}"
mkdir -p "$WORK/shots"
echo "work dir: $WORK"

# This run must OWN its node. `xerj brain` attaches to whatever already
# listens on --url, so on a taken port it would index this corpus — hostile
# email included — into somebody else's node (PR #945 review: it did, then
# died on a missing admin.key). Refuse before anything is written.
for p in "$PORT" "$((PORT + 1))" "$((PORT + 2))"; do
  if (exec 3<>"/dev/tcp/127.0.0.1/$p") 2>/dev/null; then
    echo "run.sh: port $p is already in use — refusing to run against a node this script did not start. Pass a free port: run.sh <xerj> <port>" >&2
    exit 2
  fi
done
if [ -e "$WORK/data/server.pid" ]; then
  echo "run.sh: $WORK/data/server.pid exists from an earlier run — use a fresh WORK dir" >&2
  exit 2
fi

python3 "$HERE/mkcorpus.py" "$WORK/casefile" "$REPO/landing/resources"

# `xerj brain` boots a node on $PORT (REST $PORT+1, gRPC $PORT+2) because
# nothing listens there, with auth ON, and keeps its data under --data-dir.
trap 'kill "$(cat "$WORK/data/server.pid" 2>/dev/null)" 2>/dev/null || true' EXIT
"$XERJ" brain "$WORK/casefile" --brain casefile --url "http://localhost:$PORT" \
  --data-dir "$WORK/data" --no-open --disable-feedback 2>&1 | tee "$WORK/brain.log" || true
if [ ! -s "$WORK/data/server.pid" ] || [ ! -s "$WORK/data/admin.key" ]; then
  echo "run.sh: xerj brain did not boot a node under $WORK/data (no server.pid / admin.key) — stopping" >&2
  exit 1
fi

SETUP="$(grep -o "http://[^ ]*/_xerj-console/setup#token=[^ ]*" "$WORK/brain.log" | head -1 || true)"
ADMIN_KEY="$(cat "$WORK/data/admin.key")"
BRAIN=casefile node "$HERE/live-e2e.mjs" "http://localhost:$PORT" "$ADMIN_KEY" "${SETUP:--}" "$WORK/shots" | tee "$WORK/live-e2e.json"
echo "screenshots: $WORK/shots"
