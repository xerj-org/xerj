#!/usr/bin/env bash
# Logging `xerj` shim for the XERJ arms (PLAN.md §3 note: "invoke xerj through a
# logging PATH shim that counts invocations"). Generated per run by `pov.py prepare`
# from this template; the placeholders are filled in there.
#
# Every call appends one JSON line to $HOME/.pov/xerj-calls.jsonl and keeps the
# call's stdout/stderr in $HOME/.pov/calls/<n>.{out,err} so the scorer can read
# `answered from:` / `answered_from` and the routed corpus (PLAN.md §6 routing).
# The v2 shim also points the client at the catalogue override (SMOKE.md: the live
# hub catalogue is not deployed) — those two exports are the ONLY behaviour change.
@HUB_EXPORTS@
REAL='@REAL@'
d="$HOME/.pov/calls"; mkdir -p "$d"
n=$(( $(ls "$d" 2>/dev/null | grep -c '\.rc$') + 1 ))
t0=$(date +%s.%N)
# stdout/stderr are teed, not captured, so the agent sees exactly what it would see.
# A daemon the call spawns (the hub node) is started detached with its own log, so it
# does not hold the tee pipes open.
{ "$REAL" "$@" 2> >(tee "$d/$n.err" >&2); echo $? > "$d/$n.rc"; } | tee "$d/$n.out"
rc=$(cat "$d/$n.rc" 2>/dev/null || echo 255)
t1=$(date +%s.%N)
python3 -c 'import json,sys; print(json.dumps({"n":int(sys.argv[1]),"argv":sys.argv[5:],"rc":int(sys.argv[2]),"t0":float(sys.argv[3]),"t1":float(sys.argv[4])}))' \
  "$n" "$rc" "$t0" "$t1" "$@" >> "$HOME/.pov/xerj-calls.jsonl"
exit "$rc"
