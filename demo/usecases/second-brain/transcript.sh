#!/usr/bin/env bash
# transcript.sh — the API proof for the second-brain demo, against the server
# boot-and-brain.sh started. Every claim in the case study that touches the
# HTTP surface is exercised here; every number printed is read from a live
# response, never hard-coded.
#
# Covers: overview (counts, detectors, honesty fields) · ego (evidence,
# node hydration, not_shown) · link (idempotent assert with evidence) ·
# unlink (retire, never delete) · as_of replay (the retired link is still
# believed in the past) · the two normative 400s (hops cap wording,
# self-edge) · unknown-brain 404.
#
# Idempotent: safe to re-run; the manual edge it asserts has a fixed
# valid_at, so re-runs converge on the same edge_id.
set -uo pipefail

DEMO_ROOT="${XERJ_BRAIN_DEMO_ROOT:-${TMPDIR:-/tmp}/xerj-second-brain-demo}"
PORT="${XERJ_BRAIN_DEMO_PORT:-9331}"
URL="http://localhost:$PORT"
BRAIN="vault"
KEY_FILE="$DEMO_ROOT/data/admin.key"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

AUTH=()
[ -s "$KEY_FILE" ] && AUTH=(-H "Authorization: ApiKey $(cat "$KEY_FILE")")

PASSED=0 FAILED=0
ok()   { echo "  PASS: $*"; PASSED=$((PASSED + 1)); }
bad()  { echo "  FAIL: $*"; FAILED=$((FAILED + 1)); }
ck()   { local desc="$1"; shift; if "$@" >/dev/null 2>&1; then ok "$desc"; else bad "$desc"; fi; }

# req METHOD PATH [BODY] [-G-style query pairs after --] → status in $STATUS,
# body in $WORK/resp.json
req() {
  local method="$1" path="$2" body="${3:-}"
  shift; shift; [ $# -gt 0 ] && shift
  [ "${1:-}" = "--" ] && shift   # the separator is not a query pair
  local args=(-s -o "$WORK/resp.json" -w '%{http_code}' -X "$method" "${AUTH[@]}")
  if [ -n "$body" ]; then
    args+=(-H 'Content-Type: application/json' -d "$body")
  fi
  local q=()
  for kv in "$@"; do q+=(--data-urlencode "$kv"); done
  if [ ${#q[@]} -gt 0 ]; then
    STATUS=$(curl "${args[@]}" -G "${q[@]}" "$URL$path")
  else
    STATUS=$(curl "${args[@]}" "$URL$path")
  fi
}
J() { jq -e "$@" "$WORK/resp.json"; }

echo "── second-brain API transcript · brain '$BRAIN' at $URL ──"

code=$(curl -s -o /dev/null -w '%{http_code}' "$URL/health/ready" || true)
if [ "$code" != "200" ]; then
  echo "FAIL: no server at $URL (HTTP $code) — run ./boot-and-brain.sh first"
  exit 1
fi

# ── 1. overview: the brain-level truth ───────────────────────────────────
echo "[1] GET /_graph/$BRAIN/overview"
req GET "/_graph/$BRAIN/overview"
ck "HTTP 200"                              test "$STATUS" = 200
ck "exists == true"                        J '.exists == true'
ck "contract xerj-second-brain/1"          J '.contract == "xerj-second-brain/1"'
ck "embedder honesty marker is lexical"    J '.embedder == "lexical-feature-hash"'
ck "edges.total >= 1"                      J '.edges.total >= 1'
ck "invalidated == total - live"           J '.edges.invalidated == .edges.total - .edges.live'
# Matched on the detector family, not the pinned `@N`: the contract is that
# each of these five fires on the vault, while the version is *designed* to
# move (detect/mod.rs mandates bumping @N on any behavior change), so pinning
# it here would fail every deliberate bump instead of every real regression.
for tag in wikilink mdlink href sequence samedir; do
  ck "detector $tag has live edges"        J --arg t "$tag@" '[.detectors[] | select((.detector | startswith($t)) and .live > 0)] | length == 1'
done
ck "not_shown accounting present"          J '.not_shown | has("types_not_listed") and has("hubs_out_not_listed")'
echo "  measured: $(jq -c '{edges, types: [.types[] | {(.type): .live}] | add}' "$WORK/resp.json")"

HUB=$(jq -r '.hubs.out[0].id' "$WORK/resp.json")
ck "an out-hub exists"                     test -n "$HUB" -a "$HUB" != null
echo "  hub (most outbound links): $HUB"

# ── 2. ego: one note's annotated neighborhood ────────────────────────────
echo "[2] GET /_graph/$BRAIN/ego (hub, 2 hops, nodes hydrated)"
req GET "/_graph/$BRAIN/ego" "" -- "node=$HUB" "hops=2" "include_nodes=true"
ck "HTTP 200"                              test "$STATUS" = 200
ck "contract xerj-second-brain/1"          J '.contract == "xerj-second-brain/1"'
ck "edges returned"                        J '.edges | length >= 1'
ck "every edge carries hop + direction"    J 'all(.edges[]; .hop >= 1 and (.direction == "out" or .direction == "in"))'
ck "detected edges carry evidence quotes"  J 'all(.edges[] | select(.detector != "manual@1"); .evidence.quote | type == "string")'
ck "node summaries hydrated"               J '.nodes | type == "object" and length >= 1'
ck "not_shown accounting present"          J '.not_shown | has("edges_clipped") and has("expired_excluded") and has("dangling_nodes")'
echo "  measured: $(jq -c '{edges: (.edges | length), neighbors: (.neighbors | length), not_shown}' "$WORK/resp.json")"

DST=$(jq -r '.neighbors[0].id' "$WORK/resp.json")
ck "a neighbor exists to link against"     test -n "$DST" -a "$DST" != null

# ── 3. link: idempotent assert with evidence ─────────────────────────────
echo "[3] POST /_graph/$BRAIN/link (manual edge, fixed valid_at)"
LINK_BODY=$(jq -nc --arg src "$HUB" --arg dst "$DST" '{
  src: $src, dst: $dst, type: "references",
  valid_at: "2026-07-01T00:00:00Z",
  confidence: 0.9,
  evidence: {quote: "asserted by transcript.sh for the retire-then-replay proof",
             source: "transcript.sh", offset: 0}}')
req POST "/_graph/$BRAIN/link" "$LINK_BODY"
ck "HTTP 200 or 201"                       test "$STATUS" = 200 -o "$STATUS" = 201
EDGE_ID=$(jq -r '.edge_id' "$WORK/resp.json")
ck "edge_id returned"                      test -n "$EDGE_ID" -a "$EDGE_ID" != null
req POST "/_graph/$BRAIN/link" "$LINK_BODY"
ck "identical re-assert → created:false"   J '.created == false'
ck "identical re-assert → same edge_id"    J --arg e "$EDGE_ID" '.edge_id == $e'
echo "  edge_id: $EDGE_ID"
curl -s -o /dev/null "${AUTH[@]}" -X POST "$URL/.xerj-memory-$BRAIN-edges/_refresh" || true

req GET "/_graph/$BRAIN/ego" "" -- "node=$HUB" "types=references"
ck "asserted link visible live"            J --arg e "$EDGE_ID" '[.edges[] | select(.edge_id == $e)] | length == 1'

# ── 4. unlink: retire, never delete ──────────────────────────────────────
echo "[4] DELETE /_graph/$BRAIN/link/$EDGE_ID (retire as of 2026-07-25)"
req DELETE "/_graph/$BRAIN/link/$EDGE_ID" "" -- "invalid_at=2026-07-25T00:00:00Z"
ck "HTTP 200"                              test "$STATUS" = 200
ck "invalidated == true"                   J '.invalidated == true'
ck "expired_at recorded (system clock)"    J '.expired_at | type == "number"'
curl -s -o /dev/null "${AUTH[@]}" -X POST "$URL/.xerj-memory-$BRAIN-edges/_refresh" || true

# ── 5. time travel: the retired link is still believed in the past ───────
echo "[5] as_of replay"
req GET "/_graph/$BRAIN/ego" "" -- "node=$HUB" "types=references" "as_of=2026-07-10T00:00:00Z"
ck "as_of 2026-07-10 → link WAS believed"  J --arg e "$EDGE_ID" '[.edges[] | select(.edge_id == $e)] | length == 1'
req GET "/_graph/$BRAIN/ego" "" -- "node=$HUB" "types=references"
ck "now → link no longer believed"         J --arg e "$EDGE_ID" '[.edges[] | select(.edge_id == $e)] | length == 0'
ck "…and the exclusion is COUNTED"         J '.not_shown.expired_excluded >= 1'
req GET "/_graph/$BRAIN/ego" "" -- "node=$HUB" "types=references" "include_expired=true"
ck "include_expired → still queryable, with invalid_at" \
  J --arg e "$EDGE_ID" '[.edges[] | select(.edge_id == $e and (.invalid_at | type == "number"))] | length == 1'

# ── 6. the normative refusals ────────────────────────────────────────────
echo "[6] normative errors"
req GET "/_graph/$BRAIN/ego" "" -- "node=$HUB" "hops=3"
ck "hops=3 → HTTP 400"                     test "$STATUS" = 400
ck "…with the not-a-graph-database wording" J '.error.reason | contains("hops is capped at 2") and contains("not a graph")'
req POST "/_graph/$BRAIN/link" '{"src":"a","dst":"a","type":"references"}'
ck "self-edge → HTTP 400"                  test "$STATUS" = 400
ck "…with the normative reason"            J '.error.reason | contains("self-edges are not allowed (src == dst)")'
req GET "/_graph/no-such-brain/overview"
ck "unknown brain → HTTP 404"              test "$STATUS" = 404
ck "…body says exists:false"               J '.exists == false'

# ── summary ──────────────────────────────────────────────────────────────
echo "──"
echo "transcript: $PASSED passed, $FAILED failed"
[ "$FAILED" -eq 0 ] && echo "RESULT: PASS" || echo "RESULT: FAIL"
exit $((FAILED > 0))
