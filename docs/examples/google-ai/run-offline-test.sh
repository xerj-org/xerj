#!/usr/bin/env bash
# Key-free proof of the OpenAI-compatible contract that Gemini + EmbeddingGemma
# both speak. Starts a mock /v1/embeddings server, runs XERJ in proxy mode, and
# shows the external API is called at ingest AND query time.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
XERJ="${XERJ_BIN:-$HERE/../../../engine/target/release/xerj}"; DATA="$(mktemp -d)"
cat > "$DATA/mock.toml" <<TOML
[embedding]
mode = "proxy"
default_endpoint = "http://127.0.0.1:8900/v1/embeddings"
default_model = "mock-embed-256"
TOML
python3 "$HERE/mock_embed_server.py" & MOCK=$!
trap 'kill $MOCK ${XPID:-} 2>/dev/null || true; rm -rf "$DATA"' EXIT; sleep 1
XERJ_EMBEDDING_API_KEY=sk-mock "$XERJ" --insecure --data-dir "$DATA" --config "$DATA/mock.toml" >"$DATA/x.log" 2>&1 & XPID=$!
until curl -s -m2 -o /dev/null localhost:9200/ 2>/dev/null; do sleep 1; done
grep -m1 -a "embedding backend" "$DATA/x.log"
H='Content-Type: application/json'
curl -s -XPUT localhost:9200/kb -H "$H" -d '{"mappings":{"properties":{"body":{"type":"semantic_text"}}}}' >/dev/null
curl -s localhost:9200/_bulk -H "$H" --data-binary $'{"index":{"_index":"kb","_id":"0"}}\n{"body":"the payment service went down when the connection pool was exhausted"}\n' >/dev/null
curl -s -XPOST localhost:9200/kb/_refresh >/dev/null
echo "external-API calls: $(curl -s localhost:8900/ | python3 -c 'import json,sys;print(json.load(sys.stdin)["calls"])')"
curl -s "localhost:9200/kb/_search?filter_path=hits.hits._source.body" -H "$H" -d '{"size":1,"query":{"semantic":{"field":"body","query":"database outage from too many open connections","k":1}}}'; echo
