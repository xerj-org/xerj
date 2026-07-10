#!/bin/sh
# Zero-config folder → neural semantic search, in one binary.
#
# Points `xerj autoindex` at a folder of mixed files (Markdown, plain text,
# NDJSON, CSV — see demo/data/support-folder/), lets XERJ discover the shape
# of each file and index it with NO mapping written by hand, then searches
# the prose it discovered *by meaning* using the built-in neural embedder.
#
# The one thing that makes the semantics real: the server is started with
# `--embed-mode neural`, so every `semantic_text` body autoindex creates is
# embedded in-process by the built-in BERT model (all-MiniLM-L6-v2). Drop
# that flag and the exact same recipe runs on the lexical embedder instead —
# autoindex, the mapping, and the queries do not change.
#
# Usage:  recipes/autoindex_semantic.sh [path-to-xerj-binary]
#
# Requires a xerj binary built with the neural feature:
#     cd engine && cargo build --release -p xerj-server --features neural
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
XERJ_BIN="${1:-$ROOT/engine/target/release/xerj}"
[ -x "$XERJ_BIN" ] || XERJ_BIN=$(command -v xerj) || {
  echo "error: no xerj binary. Build it with:" >&2
  echo "  cd engine && cargo build --release -p xerj-server --features neural" >&2
  exit 1
}
FOLDER="$ROOT/demo/data/support-folder"

DATA=$(mktemp -d)
ESPORT=9218
cat >"$DATA/xerj.toml" <<EOF
[server]
data_dir       = "$DATA"
rest_port      = 8318
grpc_port      = 9418
es_compat_port = $ESPORT
EOF
URL="http://127.0.0.1:${ESPORT}"

# --embed-mode neural: bodies autoindex marks semantic_text are embedded by
# the built-in BERT model. First search triggers a one-time model load
# (download on first ever use, then cached on disk).
"$XERJ_BIN" --insecure --config "$DATA/xerj.toml" --embed-mode neural \
  >"$DATA/server.log" 2>&1 &
SRV=$!
trap 'kill "$SRV" 2>/dev/null || true; wait "$SRV" 2>/dev/null || true; rm -rf "$DATA" 2>/dev/null || true' EXIT INT TERM

i=0
until curl -fsS "$URL/" >/dev/null 2>&1; do
  i=$((i + 1)); [ "$i" -gt 60 ] && { echo "server never came up"; cat "$DATA/server.log"; exit 1; }
  sleep 0.5
done

if ! grep -q "neural" "$DATA/server.log"; then
  echo "note: this binary has no neural backend — falling back to the lexical"
  echo "      embedder. Rebuild with --features neural for real neural semantics."
  echo
fi

echo "═══ 1. discover + index a folder (zero config) ═══"
# One command: walk the folder, sniff each file's format, infer a schema,
# and bulk-index. Prose bodies become semantic_text (neural-embedded here).
# --state-dir keeps the resume journal inside this throwaway run so the
# demo re-indexes cleanly every time (drop it to get real resume behaviour).
"$XERJ_BIN" autoindex "$FOLDER" --url "$URL" --state-dir "$DATA/ax-state"
echo

echo "═══ 2. the data map XERJ built for you ═══"
"$XERJ_BIN" autoindex map --url "$URL" | sed -n '1,12p'
echo "   … (full map: xerj autoindex map)"
echo

# tiny curl+python helper for pretty search output
search() { curl -fsS -X POST "$URL/$1/_search" -H 'content-type: application/json' -d "$2"; }
pp() { python3 -c '
import sys, json
d = json.load(sys.stdin)
for h in d["hits"]["hits"]:
    s = h["_source"]
    txt = (s.get("body") or s.get("summary") or "").replace(chr(10), " ")
    where = s.get("ax_path") or s.get("team") or s.get("ax_dataset") or ""
    print("   %6.3f  [%26s]  %s" % (h["_score"], where, txt[:64]))
print()'; }

echo "═══ 3. semantic search over the DISCOVERED prose (by meaning) ═══"
echo "Q: 'the site slowed down because we ran out of database connections'"
search "ax-txtprose" '{"query":{"semantic":{"field":"body","query":"the site slowed down because we ran out of database connections","k":5}},"size":2}' | pp
echo "   ↑ finds the checkout-latency postmortem — it never says 'slowed down'"
echo
echo "Q: 'I want my money back'"
search "ax-txtprose" '{"query":{"semantic":{"field":"body","query":"I want my money back","k":5}},"size":2}' | pp

echo "═══ 4. the structured files stay exact — filter the NDJSON tickets ═══"
echo "Q: open, high-priority tickets  (term filters, no embedding)"
search "ax-jsonl" '{"query":{"bool":{"filter":[{"term":{"status":"open"}},{"term":{"priority":"high"}}]}},"_source":["summary","team"],"size":5}' | pp

echo "Done. One binary discovered a mixed folder, embedded the prose with a"
echo "real neural model, and left the structured records exactly searchable."
