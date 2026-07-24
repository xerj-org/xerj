#!/usr/bin/env bash
# Reproducible, key-free proof that XERJ embeds through an external
# OpenAI-compatible /v1/embeddings API. Run from the repo root after
# `cargo build --release -p xerj-server`.
#
# It starts a tiny mock embeddings server, points XERJ at it in `proxy` mode,
# indexes a few docs, and shows that (a) XERJ called the external API for
# ingest and (b) a semantic query is embedded by the same API. Swap the mock
# for any real OpenAI-compatible endpoint (OpenAI, Gemini's OpenAI-compatible
# embeddings, a local text-embeddings server, …) by editing xerj-proxy.toml.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
XERJ="${XERJ_BIN:-$HERE/../../../engine/target/release/xerj}"
DATA="$(mktemp -d)"

python3 "$HERE/mock_embed_server.py" & MOCK=$!
trap 'kill $MOCK $XPID 2>/dev/null || true; rm -rf "$DATA"' EXIT
sleep 1

# A real provider expects a real key; the mock ignores it.
XERJ_EMBEDDING_API_KEY="${XERJ_EMBEDDING_API_KEY:-sk-mock}" \
  "$XERJ" --insecure --data-dir "$DATA" --config "$HERE/xerj-proxy.toml" > "$DATA/xerj.log" 2>&1 &
XPID=$!
until curl -s -m2 -o /dev/null localhost:9200/ 2>/dev/null; do sleep 1; done

echo "backend chosen:"; grep -m1 -a "embedding backend" "$DATA/xerj.log" || true

H='Content-Type: application/json'
curl -s -XPUT localhost:9200/notes -H "$H" \
  -d '{"mappings":{"properties":{"body":{"type":"semantic_text"}}}}' >/dev/null
curl -s localhost:9200/_bulk -H "$H" --data-binary $'{"index":{"_index":"notes","_id":"0"}}\n{"body":"the payment service went down when the connection pool was exhausted"}\n{"index":{"_index":"notes","_id":"1"}}\n{"body":"kittens should be vaccinated at eight and twelve weeks of age"}\n' >/dev/null
curl -s -XPOST localhost:9200/notes/_refresh >/dev/null

echo "external-API calls after ingest: $(curl -s localhost:8900/ | python3 -c 'import json,sys;print(json.load(sys.stdin)["calls"])')"
echo "semantic query result:"
curl -s "localhost:9200/notes/_search?filter_path=hits.hits._source.body" -H "$H" \
  -d '{"size":1,"query":{"semantic":{"field":"body","query":"database outage from too many open connections","k":1}}}'
echo
echo "external-API calls after query: $(curl -s localhost:8900/ | python3 -c 'import json,sys;print(json.load(sys.stdin)["calls"])')"
