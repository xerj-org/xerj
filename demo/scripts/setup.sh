#!/usr/bin/env bash
#
# setup.sh — bootstrap the XERJ demo environment.
#
# Builds xerj-server in release mode (cached on second run), starts it in the
# background on port 9200 (ES-compat), and waits for the cluster to report
# green. On exit the server PID is written to demo/.xerj.pid so teardown.sh
# can stop it cleanly.
#
# Run from anywhere — paths are resolved relative to this script.
#
# Usage:
#   demo/scripts/setup.sh           # build (if needed) + start
#   FAST=1 demo/scripts/setup.sh    # skip rebuild if binary already exists
#
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
WORKSPACE="$REPO/engine"
DATA_DIR="${XERJ_DATA_DIR:-/tmp/xerj-demo-data}"
PID_FILE="$REPO/demo/.xerj.pid"
LOG_FILE="$REPO/demo/.xerj.log"
BINARY="$WORKSPACE/target/release/xerj"
PORT="${XERJ_ES_PORT:-9200}"

cores="$(nproc 2>/dev/null || sysctl -n hw.ncpu 2>/dev/null || echo 4)"

echo "==> XERJ demo bootstrap"
echo "    repo:      $REPO"
echo "    data dir:  $DATA_DIR"
echo "    es port:   $PORT"

# 1. Refuse to start if already running.
if [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    echo "    server already running (pid $(cat "$PID_FILE")) — run teardown.sh first"
    exit 1
fi

# 2. Build if needed.
if [[ "${FAST:-0}" == "1" && -x "$BINARY" ]]; then
    echo "==> skipping build (FAST=1, binary present)"
else
    echo "==> building xerj-server (release, -j $cores)"
    (cd "$WORKSPACE" && cargo build --release -j "$cores" -p xerj-server)
fi

# 3. Clean data dir for a deterministic demo.
rm -rf "$DATA_DIR"
mkdir -p "$DATA_DIR"

# 4. Start the server in the background.
echo "==> starting server"
"$BINARY" --insecure --data-dir "$DATA_DIR" >"$LOG_FILE" 2>&1 &
echo $! > "$PID_FILE"

# 5. Wait for green cluster (60s budget).
echo "==> waiting for cluster green on :$PORT"
for i in $(seq 1 60); do
    if curl -fsS "http://localhost:$PORT/_cluster/health" 2>/dev/null | grep -q '"status":"green"'; then
        echo "    ready (after ${i}s)"
        break
    fi
    sleep 1
    if [[ $i -eq 60 ]]; then
        echo "    server failed to come up — last 30 log lines:"
        tail -30 "$LOG_FILE"
        kill "$(cat "$PID_FILE")" 2>/dev/null || true
        rm -f "$PID_FILE"
        exit 1
    fi
done

# 6. Print a summary.
echo
echo "==> XERJ is up"
echo "    pid:    $(cat "$PID_FILE")"
echo "    log:    $LOG_FILE"
echo "    health: curl http://localhost:$PORT/_cluster/health"
echo "    stop:   demo/scripts/teardown.sh"
