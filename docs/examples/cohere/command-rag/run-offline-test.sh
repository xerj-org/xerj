#!/usr/bin/env bash
# Key-free end-to-end test of the Cohere cookbook against a mock Cohere API.
#   1. starts the mock (mock_cohere_server.py) and a throwaway XERJ node on a private port
#   2. autoindexes the public PDFs in landing/resources
#   3. runs `command_rag.py index` then `ask --assert-citations`, once for each
#      citation form Cohere may return (custom id echo, positional doc:N)
#   4. proves the proxy embedder handles Cohere's compatibility route, whose
#      response has no per-item `index` (needs a xerj built from this branch)
# Replay-tested only: nothing here calls Cohere or measures Cohere quality.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../../../.." && pwd)"
XERJ="${XERJ_BIN:-$REPO/engine/target/release/xerj}"
PORT="${XERJ_TEST_PORT:-19641}"
MOCK_PORT="${MOCK_COHERE_PORT:-18901}"
DATA="$(mktemp -d)"
export XERJ_URL="http://127.0.0.1:$PORT" CO_BASE_URL="http://127.0.0.1:$MOCK_PORT" CO_API_KEY="mock-key"
export XERJ_DISABLE_FEEDBACK=true MOCK_COHERE_PORT="$MOCK_PORT"
MOCK_PID=""; XERJ_PID=""
cleanup() { kill $MOCK_PID $XERJ_PID 2>/dev/null || true; rm -rf "$DATA"; }
trap cleanup EXIT

start_mock() { CITATION_FORM="$1" python3 "$HERE/mock_cohere_server.py" & MOCK_PID=$!; sleep 1; }
start_xerj() {
  "$XERJ" --insecure --data-dir "$DATA/node" --no-auto-port --port "$PORT" ${1:+--config "$1"} >"$DATA/xerj.log" 2>&1 & XERJ_PID=$!
  for _ in $(seq 1 60); do curl -sf "$XERJ_URL/" >/dev/null && return 0; sleep 1; done
  echo "xerj did not start"; tail -20 "$DATA/xerj.log"; exit 1
}

echo "== cookbook, citations echo our ids"
start_mock id
start_xerj
mkdir -p "$DATA/pdfs" && cp "$REPO"/landing/resources/*.pdf "$DATA/pdfs/"
"$XERJ" autoindex "$DATA/pdfs" --url "$XERJ_URL" --prefix ck >"$DATA/autoindex.log" 2>&1
python3 "$HERE/command_rag.py" index --source ck-docs --target cohere_rag
python3 "$HERE/command_rag.py" ask "How does XERJ handle security analytics?" --assert-citations

echo "== cookbook, citations name documents by position (doc:N)"
kill $MOCK_PID; wait $MOCK_PID 2>/dev/null || true
start_mock doc
python3 "$HERE/command_rag.py" ask "What does the healthcare brief say about search?" --assert-citations
calls="$(curl -s "http://127.0.0.1:$MOCK_PORT/")"; echo "mock calls since restart: $calls"

echo "== proxy embedder on Cohere's compatibility route (no per-item index)"
kill $XERJ_PID; wait $XERJ_PID 2>/dev/null || true
cat >"$DATA/compat.toml" <<TOML
[embedding]
mode = "proxy"
default_endpoint = "http://127.0.0.1:$MOCK_PORT/compatibility/v1"
default_model = "embed-v5.0-pro"
batch_size = 64
timeout_ms = 30000
TOML
rm -rf "$DATA/node"
XERJ_EMBEDDING_API_KEY="mock-key" start_xerj "$DATA/compat.toml"
H='Content-Type: application/json'
curl -sf -XPUT "$XERJ_URL/kb" -H "$H" -d '{"mappings":{"properties":{"body":{"type":"semantic_text"}}}}' >/dev/null
bulk=$(printf '%s\n' \
  '{"index":{"_index":"kb","_id":"pool"}}' '{"body":"the payment service went down when the connection pool was exhausted"}' \
  '{"index":{"_index":"kb","_id":"cert"}}' '{"body":"the TLS certificate expired on the edge proxy at midnight"}')
r=$(curl -s "$XERJ_URL/_bulk" -H "$H" --data-binary "$bulk"$'\n')
echo "$r" | python3 -c 'import json,sys; d=json.load(sys.stdin); sys.exit("bulk errors: "+json.dumps(d)[:300] if d.get("errors") else 0)'
curl -sf -XPOST "$XERJ_URL/kb/_refresh" >/dev/null
top=$(curl -s "$XERJ_URL/kb/_search" -H "$H" -d '{"size":1,"query":{"semantic":{"field":"body","query":"database outage from too many open connections","k":1}}}' \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["hits"]["hits"][0]["_id"])')
echo "semantic top hit: $top"
[ "$top" = "pool" ] || { echo "FAIL: expected pool"; exit 1; }
echo "compat route calls: $(curl -s "http://127.0.0.1:$MOCK_PORT/" | python3 -c 'import json,sys; print(json.load(sys.stdin)["compat_embed"])')"
echo "ALL OK (replay-tested against a mock; no live Cohere call was made)"
