#!/bin/bash
# Per-index doc counts for all ax* indices on an endpoint, sorted, one per line.
# Usage: collect_counts.sh <url> [prefix]
URL="${1:?url}"; PFX="${2:-ax}"
curl -s "$URL/_refresh" > /dev/null
curl -s "$URL/_cat/indices" | awk -v p="$PFX" '$3 ~ "^"p {print $3}' | sort | while read -r idx; do
  c=$(curl -s "$URL/$idx/_count" | python3 -c 'import sys,json;print(json.load(sys.stdin)["count"])')
  echo "$idx $c"
done
