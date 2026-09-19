#!/usr/bin/env bash
# Measure `xerj autoindex` on a synthetic Google Takeout mailbox.
#
#   benchmarks/mbox-ingest/run.sh <xerj-binary> <work-dir> <profile> <size> <es-port>
#   e.g.  run.sh engine/target/release/xerj /data/mboxbench mixed 1G 9520
#
#   AX_FLAGS="--no-semantic"   extra `xerj autoindex` flags for this run
#   LABEL=no-semantic          suffix for the result file (default: "default")
#   RESUME=1                   keep the node's data dir and the state dir from the
#                              previous run with this LABEL and run the SAME command
#                              again - what the tool tells you to do after a failure
#   XERJ_MAX_PROCESS_MEMORY_MB=off   inherited by the node: lifts the server's
#                              auto memory cap. Spell it "off": a bare 0 is
#                              ambiguous to the governor and silently ignored
#                              (one run here was labelled "uncapped" and was not).
#
# <work-dir> MUST be on a real disk. On a tmpfs (/tmp on many distros) the
# corpus, the index and the staging file all live in RAM, and every memory
# figure this prints would be wrong.
#
#   VERIFY_PER_KIND=0          needles verify.py checks per kind (0 = every planted
#                              needle, the default here; the published 1 GB figures
#                              say which was used)
#
# It boots a THROWAWAY node on <es-port> (+1 rest, +2 grpc) with its own data
# directory, the default LEXICAL embedder and AUTH ON (the node writes its admin
# key to <data-dir>/admin.key; every request below sends it), indexes the tree
# with default autoindex settings, and writes
# <work-dir>/result-<profile>-<size>.json. Nothing here talks to any other node:
# before the first write it checks that the listener on <es-port> is the process
# it just started.
set -euo pipefail
XERJ=$(readlink -f "$1"); WORK=$(readlink -f "$2"); PROFILE=$3; SIZE=$4; PORT=$5
HERE=$(cd "$(dirname "$0")" && pwd); REPO=$(cd "$HERE/../.." && pwd)
LABEL=${LABEL:-default}; AX_FLAGS=${AX_FLAGS:-}; RESUME=${RESUME:-0}
SRC="$PROFILE-$SIZE"; TREE="$WORK/tree-$SRC"; TRUTH="$WORK/tree-$SRC.truth.json"
TAG="$SRC-$LABEL"; [ "$RESUME" = 1 ] && RUN="$TAG-resume" || RUN="$TAG"
DATA="$WORK/node-$TAG"; STATE="$WORK/state-$TAG"; OUT="$WORK/result-$RUN.json"
URL="http://127.0.0.1:$PORT"

case "$(findmnt -no FSTYPE -T "$WORK")" in tmpfs|ramfs) echo "refusing: $WORK is RAM-backed"; exit 2;; esac
if ss -ltn | grep -qE ":($PORT|$((PORT+1))|$((PORT+2)))\b"; then echo "refusing: port block $PORT.. is in use"; exit 2; fi

if [ ! -d "$TREE" ]; then
  python3 "$REPO/scripts/synthetic-takeout.py" --out "$TREE" --truth "$TRUTH" \
      --target-bytes "$SIZE" --profile "$PROFILE" --seed 42 --with-archive
fi
if [ "$RESUME" != 1 ]; then rm -rf "$DATA" "$STATE"; fi
mkdir -p "$DATA"
cat > "$WORK/node-$TAG.toml" <<TOML
[server]
es_compat_port = $PORT
rest_port = $((PORT+1))
grpc_port = $((PORT+2))
data_dir = "$DATA"
[tls]
enabled = false
[embedding]
mode = "lexical"
TOML
nohup "$XERJ" -c "$WORK/node-$TAG.toml" --embed-mode lexical > "$WORK/server-$RUN.log" 2>&1 &
SERVER=$!
trap 'kill $SERVER 2>/dev/null || true' EXIT
KEY=""
for _ in $(seq 1 120); do
  [ -s "$DATA/admin.key" ] && KEY=$(cat "$DATA/admin.key")
  [ -n "$KEY" ] && curl -fsS -H "Authorization: ApiKey $KEY" "$URL/_cluster/health" >/dev/null 2>&1 && break
  sleep 0.5
done
curl -fsS -H "Authorization: ApiKey $KEY" "$URL/_cluster/health" >/dev/null || { echo "node did not come up"; exit 1; }
# A node that failed to bind plus a curl that succeeds means SOMEBODY ELSE's node
# answered. Never write into it.
LISTENER=$(ss -ltnp 2>/dev/null | grep -E ":$PORT\b" | grep -o 'pid=[0-9]*' | head -1 | cut -d= -f2)
[ "$LISTENER" = "$SERVER" ] || { echo "refusing: :$PORT is held by pid ${LISTENER:-?}, not by the node this script started ($SERVER)"; exit 2; }

hwm() { awk '/^VmHWM:/{print $2}' "/proc/$1/status" 2>/dev/null || echo 0; }   # kB
SERVER_IDLE_KB=$(hwm $SERVER)
LOAD_BEFORE=$(cut -d' ' -f1-3 /proc/loadavg)

# ── the run ── default settings; --yes answers the >10-minute estimate gate.
# Run under GNU time: its "Maximum resident set size" is the largest SINGLE
# process in the tree — autoindex itself or any PDF worker child it waited for
# (getrusage RUSAGE_CHILDREN). PDF workers live ~50 ms each, so polling cannot
# see them; this can.
START=$(date +%s.%N)
# The key goes by environment, not argv: argv is world-readable in /proc.
XERJ_API_KEY="$KEY" /usr/bin/time -v -o "$WORK/time-$RUN.txt" \
  "$XERJ" autoindex "$TREE" --url "$URL" --prefix bench --brain bench --state-dir "$STATE" \
    --yes --progress plain --json $AX_FLAGS > "$WORK/autoindex-$RUN.json" 2> "$WORK/autoindex-$RUN.log" &
TIMEPID=$!
# time's only child is autoindex. No `-x xerj`: a binary copied under another
# name (xerj-final) never matched, and autoindex_process was recorded as 0.0.
AX=""; for _ in $(seq 1 50); do AX=$(pgrep -P $TIMEPID 2>/dev/null | head -1 || true); [ -n "$AX" ] && break; sleep 0.1; done
# The autoindex process's OWN peak: VmHWM is a high-water mark, so the last
# sample before exit is its peak to within one interval.
AX_KB=0; STATE_B=0
while kill -0 $TIMEPID 2>/dev/null; do
  if [ -n "$AX" ]; then v=$(hwm $AX); [ "${v:-0}" -gt "$AX_KB" ] && AX_KB=$v; fi
  b=$(du -sb "$STATE" 2>/dev/null | cut -f1 || echo 0); [ "${b:-0}" -gt "$STATE_B" ] && STATE_B=$b
  sleep 0.5
done
set +e; wait $TIMEPID; RC=$?; set -e
END=$(date +%s.%N)
KIDS_KB=$(awk -F': ' '/Maximum resident set size/{print $2}' "$WORK/time-$RUN.txt")
# Two server figures, because they answer different questions: the peak while
# autoindex was sending (what the ingest itself cost) and the peak after the
# post-run flush and merges below (what the node needed in total).
SERVER_AT_EXIT_KB=$(hwm $SERVER)
INDEX_B_AT_EXIT=$(du -sb "$DATA" | cut -f1)

# Let flush + background merges settle: size is stable for 30 s.
curl -fsS -H "Authorization: ApiKey $KEY" -XPOST "$URL/_flush" >/dev/null 2>&1 || true
prev=-1; stable=0
for _ in $(seq 1 120); do
  cur=$(du -sb "$DATA" | cut -f1)
  if [ "$cur" = "$prev" ]; then stable=$((stable+1)); else stable=0; fi
  [ "$stable" -ge 6 ] && break; prev=$cur; sleep 5
done
INDEX_B_SETTLED=$(du -sb "$DATA" | cut -f1)
SERVER_PEAK_KB=$(hwm $SERVER)

XERJ_API_KEY="$KEY" python3 "$HERE/verify.py" --url "$URL" --prefix bench --brain bench --truth "$TRUTH" \
    --per-kind "${VERIFY_PER_KIND:-0}" > "$WORK/verify-$RUN.json" || true

python3 - "$OUT" <<PY
import json, os, sys
def load(p):
    try: return json.load(open(p))
    except Exception as e: return {"unreadable": str(e)}
truth = load("$TRUTH"); ver = load("$WORK/verify-$RUN.json")
tree_bytes = sum(os.path.getsize(os.path.join(d, f)) for d, _, fs in os.walk("$TREE") for f in fs)
wall = $END - $START
docs = ver.get("node_docs", 0)
json.dump({
  "profile": "$PROFILE", "target_size": "$SIZE", "label": "$LABEL", "extra_autoindex_flags": "$AX_FLAGS",
  "resumed_after_a_failed_run": "$RESUME" == "1", "autoindex_exit_code": $RC,
  "autoindex_last_line": open("$WORK/autoindex-$RUN.log", errors="replace").read().strip().splitlines()[-1][:600],
  "source": {"mbox_bytes": truth["mbox"]["bytes"], "tree_bytes": tree_bytes, "entries": truth["entries"],
             "attachments": truth["attachments"], "mbox_sha256": truth["mbox"]["sha256"]},
  "wall_seconds": round(wall, 1),
  "node_docs_indexed": docs,
  "docs_per_second": round(docs / wall, 1) if wall else None,
  "source_mb_per_second": round(truth["mbox"]["bytes"] / 1e6 / wall, 2) if wall else None,
  "peak_rss_unit": "MiB (VmHWM kB / 1024) - the key says mb for compatibility with earlier result files",
  "peak_rss_mb": {"autoindex_process": round($AX_KB / 1024, 1),
                  "largest_single_process_in_autoindex_tree": round($KIDS_KB / 1024, 1),
                  "server": round($SERVER_PEAK_KB / 1024, 1),
                  "server_at_autoindex_exit": round($SERVER_AT_EXIT_KB / 1024, 1),
                  "server_idle_before_run": round($SERVER_IDLE_KB / 1024, 1)},
  "server_memory_cap": os.environ.get("XERJ_MAX_PROCESS_MEMORY_MB") or "auto tier (see the node log)",
  "server_breaker_engagements": open("$WORK/server-$RUN.log", errors="replace").read().count("engaging the parent memory circuit breaker"),
  "disk_bytes": {"index_at_autoindex_exit": $INDEX_B_AT_EXIT, "index_settled": $INDEX_B_SETTLED,
                 "index_settled_over_mbox": round($INDEX_B_SETTLED / truth["mbox"]["bytes"], 3),
                 "state_dir_peak_during_run": $STATE_B},
  "loadavg_before": "$LOAD_BEFORE", "loadavg_after": open("/proc/loadavg").read().split()[:3],
  "verify": ver,
}, open(sys.argv[1], "w"), indent=1)
print(open(sys.argv[1]).read())
PY
