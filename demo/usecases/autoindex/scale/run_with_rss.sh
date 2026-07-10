#!/bin/bash
# Run xerj autoindex while polling /proc/<pid>/status VmRSS every 1s.
# Usage: run_with_rss.sh <rss_csv_out> <log_out> -- <cmd...>
set -u
RSS_OUT="$1"; LOG_OUT="$2"; shift 3   # skip the --
"$@" >"$LOG_OUT" 2>&1 &
PID=$!
echo "pid=$PID"
echo "t_epoch,rss_kb" > "$RSS_OUT"
while kill -0 $PID 2>/dev/null; do
  RSS=$(awk '/VmRSS/{print $2}' /proc/$PID/status 2>/dev/null)
  [ -n "${RSS:-}" ] && echo "$(date +%s.%N),$RSS" >> "$RSS_OUT"
  sleep 1
done
wait $PID
echo "exit_code=$?"
