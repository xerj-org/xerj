#!/usr/bin/env bash
# The Reader over a mailbox file, against a REAL xerj node (auth on).
#
#   ./run-mbox.sh <path-to-xerj-binary> [port]     # default port 9570 (+1, +2)
#
# The binary must carry autoindex's mbox extractor (PR #949); built from this
# branch alone it indexes no mailbox messages and mbox-repro.mjs stops.
# Output: mbox-repro.json. Needs Node >= 22, Chrome (or CHROME_BIN), python3.
# Everything is written under $WORK; the node is stopped at the end. Refuses a
# port that is already in use.
set -euo pipefail
XERJ="${1:?usage: run-mbox.sh <xerj binary> [port]}"
PORT="${2:-9570}"
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK="${WORK:-$(mktemp -d)}"
mkdir -p "$WORK"
for p in "$PORT" "$((PORT + 1))" "$((PORT + 2))"; do
  if (exec 3<>"/dev/tcp/127.0.0.1/$p") 2>/dev/null; then
    echo "run-mbox.sh: port $p is already in use — refusing to run against a node this script did not start" >&2
    exit 2
  fi
done
[ ! -e "$WORK/data/server.pid" ] || { echo "run-mbox.sh: $WORK/data/server.pid exists — use a fresh WORK dir" >&2; exit 2; }
python3 "$HERE/mkmbox.py" "$WORK/mailbox"
trap 'kill "$(cat "$WORK/data/server.pid" 2>/dev/null)" 2>/dev/null || true' EXIT
"$XERJ" brain "$WORK/mailbox" --brain mboxrepro --url "http://localhost:$PORT" \
  --data-dir "$WORK/data" --no-open --disable-feedback 2>&1 | tee "$WORK/brain.log" || true
[ -s "$WORK/data/server.pid" ] && [ -s "$WORK/data/admin.key" ] || { echo "run-mbox.sh: xerj brain did not boot a node under $WORK/data" >&2; exit 1; }
BRAIN=mboxrepro node "$HERE/mbox-repro.mjs" "http://localhost:$PORT" "$(cat "$WORK/data/admin.key")" "$WORK/mailbox.truth.json" | tee "$WORK/mbox-repro.json"
