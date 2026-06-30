#!/usr/bin/env bash
#
# teardown.sh — stop the XERJ demo server and clean up.
#
# Sends SIGTERM to the pid recorded by setup.sh and waits up to 10s for
# graceful shutdown (XERJ's documented SIGTERM time is ~0.24s). Falls back
# to SIGKILL if the process is still alive after the grace window.
#
# Pass --purge to also delete the data directory.
#
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
PID_FILE="$REPO/demo/.xerj.pid"
LOG_FILE="$REPO/demo/.xerj.log"
DATA_DIR="${XERJ_DATA_DIR:-/tmp/xerj-demo-data}"

if [[ ! -f "$PID_FILE" ]]; then
    echo "==> no pid file at $PID_FILE — nothing to stop"
else
    pid="$(cat "$PID_FILE")"
    if kill -0 "$pid" 2>/dev/null; then
        echo "==> sending SIGTERM to xerj (pid $pid)"
        kill -TERM "$pid"
        for i in $(seq 1 10); do
            if ! kill -0 "$pid" 2>/dev/null; then
                echo "    stopped after ${i}s"
                break
            fi
            sleep 1
        done
        if kill -0 "$pid" 2>/dev/null; then
            echo "    still alive after 10s — sending SIGKILL"
            kill -KILL "$pid" || true
        fi
    else
        echo "==> pid $pid is not running (stale pid file)"
    fi
    rm -f "$PID_FILE"
fi

if [[ "${1:-}" == "--purge" ]]; then
    echo "==> purging data dir $DATA_DIR"
    rm -rf "$DATA_DIR"
    rm -f "$LOG_FILE"
fi

echo "==> done"
