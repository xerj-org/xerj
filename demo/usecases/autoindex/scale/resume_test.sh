#!/bin/bash
# Resume proof: start autoindex against a FRESH server data dir + fresh state dir,
# kill -9 mid-run, re-run the same command, verify RESUME log + identical counts.
# Usage: resume_test.sh <kill_after_seconds>
set -u
BIN=/home/claude/ai/xerj-autoindex-wt/engine/target/release/xerj
URL=http://127.0.0.1:9270
STATE=/home/claude/xerj-autoindex-scale/state-run2
CORPUS=/home/claude/xerj-autoindex-scale/corpus
OUT=/tmp/xerj-autoindex
KILL_AFTER="${1:-60}"

$BIN autoindex "$CORPUS" --url "$URL" --state-dir "$STATE" > "$OUT/ax-run2a.log" 2>&1 &
PID=$!
echo "first attempt pid=$PID (will kill -9 after ${KILL_AFTER}s)"
sleep "$KILL_AFTER"
kill -9 $PID
wait $PID 2>/dev/null
echo "killed with SIGKILL at $(date -u +%H:%M:%S)"
sleep 2
echo "--- journal state after kill ---"
wc -l "$STATE"/journal.ndjson
grep -c '"kind":"file_done"' "$STATE"/journal.ndjson || true
echo "--- counts at kill time ---"
/home/claude/ai/xerj-autoindex-wt/demo/usecases/autoindex/scale/collect_counts.sh "$URL" > "$OUT/counts-at-kill.txt"
cat "$OUT/counts-at-kill.txt"
echo "--- re-running (must RESUME) ---"
$BIN autoindex "$CORPUS" --url "$URL" --state-dir "$STATE" > "$OUT/ax-run2b.log" 2>&1
echo "rerun exit=$?"
head -5 "$OUT/ax-run2b.log"
