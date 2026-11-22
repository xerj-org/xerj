#!/bin/bash
# usage: fixture_ranks.sh <xerj> <url> <corpus> "<query>"...
# Prints the distinct ax_path order `xerj code --json` returns for each query.
X=$1; U=$2; C=$3; shift 3
for q in "$@"; do
  printf '%s\t' "$q"
  "$X" code "$C" "$q" --json -k 5 --url "$U" 2>/dev/null | python3 -c '
import json, sys
seen = []
for h in json.load(sys.stdin)["hits"]["hits"]:
    p = h["_source"].get("ax_path")
    if p and p not in seen:
        seen.append(p)
print(" > ".join(seen))'
done
