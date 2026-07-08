#!/bin/sh
# Smoke-run every recipe against a throwaway XERJ instance.
# Usage:  recipes/run_all.sh [path-to-xerj-binary]
# Exits non-zero if any recipe fails. Used by CI and as a one-shot demo.
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
XERJ_BIN="${1:-$ROOT/engine/target/release/xerj}"
[ -x "$XERJ_BIN" ] || XERJ_BIN=$(command -v xerj) || {
  echo "error: no xerj binary (build with: cargo build --release -p xerj-server)" >&2
  exit 1
}

DATA=$(mktemp -d)
# Isolated ports (config-only; the binary has no port flags) so a smoke
# run never collides with a dev instance on 8080/9200/9300.
ESPORT=9219
cat >"$DATA/xerj.toml" <<EOF
[server]
data_dir       = "$DATA"
rest_port      = 8319
grpc_port      = 9419
es_compat_port = $ESPORT
EOF
export XERJ_URL="http://127.0.0.1:${ESPORT}"

"$XERJ_BIN" --insecure --config "$DATA/xerj.toml" >"$DATA/server.log" 2>&1 &
SRV=$!
trap 'kill "$SRV" 2>/dev/null || true; wait "$SRV" 2>/dev/null || true; rm -rf "$DATA" 2>/dev/null || true' EXIT INT TERM

i=0
until curl -fsS "$XERJ_URL/" >/dev/null 2>&1; do
  i=$((i + 1)); [ "$i" -gt 60 ] && { echo "server never came up"; cat "$DATA/server.log"; exit 1; }
  sleep 0.5
done

rc=0
for r in semantic_search rag_app memory_agent log_anomaly anomaly_datafeed vector_quantization; do
  printf '\n═══ %s ═══\n' "$r"
  if python3 "$ROOT/recipes/$r.py"; then :; else echo "FAILED: $r"; rc=1; fi
done
exit "$rc"
