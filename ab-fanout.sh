#!/usr/bin/env bash
# A/B: fan-out latency on the 41-dataset bench corpus. $1 = xerj binary, $2 = port
set -euo pipefail
BIN="$1"; PORT="$2"; SP="$(dirname "$0")/.."
DATA="$3"
"$BIN" -c <(printf '[server]\nbind_address="127.0.0.1"\nes_compat_port=%s\nrest_port=%s\ngrpc_port=%s\n' "$PORT" "$((PORT+1))" "$((PORT+2))") \
  -d "$DATA" --insecure > "$DATA/../server-$PORT.log" 2>&1 &
PID=$!
for i in $(seq 1 240); do curl -s --max-time 2 "http://127.0.0.1:$PORT/_cluster/health" | grep -q '"status"' && break; sleep 1; done
lat() { for i in $(seq 1 21); do curl -s -o /dev/null -w "%{time_total}\n" -XPOST "http://127.0.0.1:$PORT/$1/_search" -H 'content-type: application/json' -d "$2"; done | sort -n | awk -v n="$3" 'NR==11{printf "%-30s p50 %7.1f ms\n", n, $1*1000}'; }
IDENT='{"size":10,"query":{"multi_match":{"query":"roaring_bitmap_and","fields":["defs^3","body","title"]}}}'
COMMON='{"size":10,"query":{"multi_match":{"query":"serialize","fields":["defs^3","body","title"]}}}'
PHRASE='{"size":10,"query":{"match_phrase":{"body":"static inline int"}}}'
lat "ax-tantivy-benches-hdfs" "$IDENT" "single ident"
lat "ax-*" "$IDENT" "fanout ident"
lat "ax-*" "$COMMON" "fanout common"
lat "ax-*" "$PHRASE" "fanout phrase"
echo "hits sanity:"; curl -s -XPOST "http://127.0.0.1:$PORT/ax-*/_search" -H 'content-type: application/json' -d "$IDENT" | python3 -c "import json,sys; d=json.load(sys.stdin); print(' total', d['hits']['total']['value'], '| top', [(h['_index'],round(h['_score'],3)) for h in d['hits']['hits'][:3]])"
kill $PID 2>/dev/null; wait $PID 2>/dev/null || true
