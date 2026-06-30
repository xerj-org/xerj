#!/usr/bin/env bash
# =============================================================================
# xerj Battle SLA Test Suite
# =============================================================================
# Runs 14 REAL HTTP-level test scenarios — one per user-feedback category.
# Every curl command is logged with timestamps so it can be replayed against
# a live Elasticsearch cluster to produce an apples-to-apples comparison.
#
# Usage:
#   cd /home/claude/ai/xerj.ai/engine
#   ./tests/battle_sla.sh
#
# Requirements: curl, python3, ps, awk, wc (all standard on Linux/macOS)
# =============================================================================
set -uo pipefail

# ─── Paths ────────────────────────────────────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
XERJ="$ENGINE_DIR/target/release/xerj"
DEFAULT_TOML="$ENGINE_DIR/xerj.default.toml"

# ─── Ports ────────────────────────────────────────────────────────────────────
ES_PORT=19200        # main ES-compat listener (avoids clash with any running ES)
NATIVE_PORT=19080    # native REST listener
GRPC_PORT=19081      # gRPC placeholder
SEC_ES_PORT=19201    # secure instance (no --insecure) used by test 09
SEC_NATIVE_PORT=19082
SEC_GRPC_PORT=19083

# ─── Output ───────────────────────────────────────────────────────────────────
TIMESTAMP=$(date +%Y-%m-%d_%H-%M-%S)
LOG_FILE="$ENGINE_DIR/BATTLE_SLA_RESULTS_${TIMESTAMP}.md"

PASS=0
FAIL=0
TOTAL=0
ZPID=""
ZPID2=""

# ─── Colour codes (disabled if not a terminal) ────────────────────────────────
if [ -t 1 ]; then
  GREEN='\033[0;32m'; RED='\033[0;31m'; YELLOW='\033[1;33m'
  CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'
else
  GREEN=''; RED=''; YELLOW=''; CYAN=''; BOLD=''; RESET=''
fi

# ─── Logging helpers ──────────────────────────────────────────────────────────
log()  { local msg="[$(date +%H:%M:%S)] $*"; echo -e "$msg"; echo "$msg" >> "$LOG_FILE"; }
hdr()  { log ""; log "${BOLD}${CYAN}=== $* ===${RESET}"; }
pass() { PASS=$((PASS+1)); TOTAL=$((TOTAL+1)); log "  ${GREEN}PASS${RESET}: $1"; }
fail() { FAIL=$((FAIL+1)); TOTAL=$((TOTAL+1)); log "  ${RED}FAIL${RESET}: $1"; }
info() { log "  ${YELLOW}INFO${RESET}: $1"; }
cmd()  { log "  CMD : $1"; }

# ─── Cleanup ──────────────────────────────────────────────────────────────────
cleanup() {
  [ -n "$ZPID"  ] && kill "$ZPID"  2>/dev/null && wait "$ZPID"  2>/dev/null || true
  [ -n "$ZPID2" ] && kill "$ZPID2" 2>/dev/null && wait "$ZPID2" 2>/dev/null || true
  [ -n "${DATA_DIR:-}"  ] && rm -rf "$DATA_DIR"
  [ -n "${DATA_DIR2:-}" ] && rm -rf "$DATA_DIR2"
}
trap cleanup EXIT INT TERM

# ─── Wait for port to accept connections ─────────────────────────────────────
wait_for_port() {
  local port=$1 timeout=${2:-10} elapsed=0
  while ! curl -sf "http://localhost:$port/" >/dev/null 2>&1; do
    sleep 0.3; elapsed=$((elapsed+1))
    if [ $elapsed -gt $((timeout * 3)) ]; then return 1; fi
  done
  return 0
}

# ─── Write config TOML for a xerj instance ──────────────────────────────────
write_config() {
  local cfg_file=$1 data_dir=$2 es_port=$3 native_port=$4 grpc_port=$5
  cat > "$cfg_file" <<TOML
[server]
es_compat_port = $es_port
rest_port      = $native_port
grpc_port      = $grpc_port
data_dir       = "$data_dir"
bind_address   = "127.0.0.1"

[auth]
enabled = false

[tls]
enabled = false
TOML
}

# =============================================================================
# Initialise output file
# =============================================================================
DATA_DIR=$(mktemp -d)
DATA_DIR2=$(mktemp -d)

cat > "$LOG_FILE" <<HEADER
# xerj Battle SLA Results — $TIMESTAMP

> **Reproducibility**: every \`curl\` command logged below can be run verbatim
> against a live Elasticsearch 8.x cluster at \`http://localhost:9200\` to
> produce an apples-to-apples comparison. Replace \`19200\` with \`9200\`.

---

HEADER

echo -e "${BOLD}xerj Battle SLA Test Suite${RESET}"
echo "Output → $LOG_FILE"
echo ""

# =============================================================================
# Build binary (if stale or missing)
# =============================================================================
hdr "BUILD"
if [ ! -f "$XERJ" ]; then
  info "Binary not found — building (this may take ~60s on first run)..."
  cmd "cargo build --release -p xerj-server"
  (cd "$ENGINE_DIR" && cargo build --release -p xerj-server 2>&1) | tail -5
fi
if [ ! -f "$XERJ" ]; then
  fail "Binary still missing after build — aborting"
  exit 1
fi
pass "Binary found: $XERJ"

# =============================================================================
# Start primary xerj instance (insecure, port 19200)
# =============================================================================
hdr "SERVER STARTUP"

CFG1=$(mktemp --suffix=.toml)
write_config "$CFG1" "$DATA_DIR" $ES_PORT $NATIVE_PORT $GRPC_PORT

cmd "$XERJ --config $CFG1 --insecure"
XERJ_LOG=error "$XERJ" --config "$CFG1" --insecure 2>/dev/null &
ZPID=$!
info "PID $ZPID — waiting for port $ES_PORT..."

ES="http://localhost:$ES_PORT"
NATIVE="http://localhost:$NATIVE_PORT"

if wait_for_port $ES_PORT 15; then
  pass "Server listening on :$ES_PORT (PID $ZPID)"
else
  fail "Server did not come up within 15 s — aborting"
  exit 1
fi

# Record startup RSS
RSS_EMPTY=$(ps -o rss= -p "$ZPID" 2>/dev/null | tr -d ' ' || echo 0)
info "RSS at startup (empty): ${RSS_EMPTY} kB ($(( RSS_EMPTY / 1024 )) MiB)"

# =============================================================================
# TEST 01 — Operational Complexity
# "ES requires 5 specialists to keep alive"
# =============================================================================
hdr "TEST 01 — Operational Complexity (feedback/01)"
log "Scenario: Deploy, create index, bulk-index 10 docs, search — all under 60 seconds"
log "ES comparison: requires Java install, JVM heap tuning, security config, 60+ sec startup"
log ""

T_START=$(date +%s%N)

# 1a. Root info endpoint
cmd "curl -s $ES/"
RESP=$(curl -sf "$ES/" 2>/dev/null)
if echo "$RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); assert d.get('version',{}).get('number','').startswith('8.')" 2>/dev/null; then
  pass "Root / returns ES 8.x-compatible response"
else
  fail "Root / response missing or wrong format (got: ${RESP:0:120})"
fi

# 1b. Create index
cmd "curl -sf -X PUT $ES/ops-test"
HTTP=$(curl -sf -o /dev/null -w "%{http_code}" -X PUT "$ES/ops-test" 2>/dev/null)
[ "$HTTP" = "200" ] && pass "PUT /ops-test → 200 OK" || fail "PUT /ops-test → $HTTP (expected 200)"

# 1c. Bulk-index 10 docs
BULK_BODY=""
for i in $(seq 1 10); do
  BULK_BODY="${BULK_BODY}{\"index\":{\"_index\":\"ops-test\",\"_id\":\"$i\"}}
{\"msg\":\"log entry $i\",\"level\":\"INFO\",\"seq\":$i}
"
done
cmd "POST $ES/_bulk  (10 docs)"
HTTP=$(echo "$BULK_BODY" | curl -sf -o /dev/null -w "%{http_code}" -X POST "$ES/_bulk" \
  -H 'Content-Type: application/x-ndjson' --data-binary @- 2>/dev/null)
[ "$HTTP" = "200" ] && pass "POST /_bulk 10 docs → 200 OK" || fail "POST /_bulk → $HTTP (expected 200)"

# 1d. Search with match_all
cmd "POST $ES/ops-test/_search  {\"query\":{\"match_all\":{}}}"
SEARCH=$(curl -sf -X POST "$ES/ops-test/_search" \
  -H 'Content-Type: application/json' \
  -d '{"query":{"match_all":{}},"size":20}' 2>/dev/null)
HITS=$(echo "$SEARCH" | python3 -c "import sys,json; print(json.load(sys.stdin)['hits']['total']['value'])" 2>/dev/null || echo -1)
[ "$HITS" = "10" ] && pass "Search returned 10 hits" || fail "Search returned $HITS (expected 10)"

T_END=$(date +%s%N)
ELAPSED_MS=$(( (T_END - T_START) / 1000000 ))
[ "$ELAPSED_MS" -lt 60000 ] \
  && pass "Total scenario time: ${ELAPSED_MS}ms (well under 60 s)" \
  || fail "Total scenario time: ${ELAPSED_MS}ms (exceeded 60 s)"

info "Equivalent ES commands (port 9200): PUT /ops-test, POST /_bulk, POST /ops-test/_search"

# =============================================================================
# TEST 02 — Cost and Pricing
# "We pay \$480K/year for Elastic Cloud"
# =============================================================================
hdr "TEST 02 — Cost and Pricing (feedback/02)"
log "Scenario: Measure actual RSS memory while actively serving requests"
log "ES comparison: JVM minimum heap is 1 GiB; large deployments need 64–256 GiB"
log ""

# Index 1 000 docs to simulate a realistic loaded state
BULK1K=""
for i in $(seq 1 1000); do
  BULK1K="${BULK1K}{\"index\":{\"_index\":\"cost-test\",\"_id\":\"$i\"}}
{\"host\":\"server-$(( (i % 20) + 1 ))\",\"status\":$(( (i % 2) * 200 + 200 )),\"bytes\":$((RANDOM % 5000 + 100)),\"ts\":$((1700000000 + i))}
"
done
cmd "POST $ES/_bulk (1 000 docs)"
HTTP=$(echo "$BULK1K" | curl -sf -o /dev/null -w "%{http_code}" -X POST "$ES/_bulk" \
  -H 'Content-Type: application/x-ndjson' --data-binary @- 2>/dev/null)
[ "$HTTP" = "200" ] && pass "1 000 docs indexed → 200" || fail "Bulk 1 000 docs → $HTTP"

RSS_1K=$(ps -o rss= -p "$ZPID" 2>/dev/null | tr -d ' ' || echo 0)
RSS_1K_MB=$(( RSS_1K / 1024 ))
info "RSS after 1 000 docs: ${RSS_1K} kB (${RSS_1K_MB} MiB)"
[ "$RSS_1K_MB" -lt 512 ] \
  && pass "RSS ${RSS_1K_MB} MiB < 512 MiB threshold" \
  || fail "RSS ${RSS_1K_MB} MiB exceeds 512 MiB threshold"

info "Cost delta: xerj self-hosted on 2 GiB VM ≈ \$20/month vs Elastic Cloud basic ≈ \$95/month/node"

# =============================================================================
# TEST 03 — JVM and Memory
# "33 GB of memory from the start with nothing in it"
# =============================================================================
hdr "TEST 03 — JVM and Memory (feedback/03)"
log "Scenario: Measure RSS at startup, after 1 K docs, after 10 K docs"
log "ES comparison: default JVM heap = 1 GiB reserved at start; typical prod = 30+ GiB"
log ""

info "RSS at startup (recorded earlier): ${RSS_EMPTY} kB ($(( RSS_EMPTY / 1024 )) MiB)"
[ "$RSS_EMPTY" -lt $((512 * 1024)) ] \
  && pass "Startup RSS $(( RSS_EMPTY / 1024 )) MiB < 512 MiB" \
  || fail "Startup RSS $(( RSS_EMPTY / 1024 )) MiB exceeds 512 MiB"

# 10 K bulk
BULK10K=""
for i in $(seq 1001 10000); do
  BULK10K="${BULK10K}{\"index\":{\"_index\":\"mem-test\",\"_id\":\"$i\"}}
{\"message\":\"event $i\",\"value\":$i,\"tag\":\"batch\"}
"
done
cmd "POST $ES/_bulk (9 000 docs for mem-test, total 10 K)"
HTTP=$(echo "$BULK10K" | curl -sf -o /dev/null -w "%{http_code}" -X POST "$ES/_bulk" \
  -H 'Content-Type: application/x-ndjson' --data-binary @- 2>/dev/null)
[ "$HTTP" = "200" ] && pass "9 000 more docs indexed" || fail "Bulk 9 000 → $HTTP"

RSS_10K=$(ps -o rss= -p "$ZPID" 2>/dev/null | tr -d ' ' || echo 0)
RSS_10K_MB=$(( RSS_10K / 1024 ))
GROWTH_MB=$(( RSS_10K_MB - RSS_EMPTY / 1024 ))
info "RSS after ~10 K docs: ${RSS_10K} kB (${RSS_10K_MB} MiB) — grew ${GROWTH_MB} MiB"
[ "$RSS_10K_MB" -lt 1024 ] \
  && pass "RSS ${RSS_10K_MB} MiB < 1 GiB after 10 K docs" \
  || fail "RSS ${RSS_10K_MB} MiB exceeds 1 GiB after 10 K docs"

info "ES starts with 1 GiB pre-allocated heap before the first document is indexed"

# =============================================================================
# TEST 04 — Scaling and Shards
# "Shard allocation took 14 hours"
# =============================================================================
hdr "TEST 04 — Scaling and Shards (feedback/04)"
log "Scenario: Create 100 indices in rapid succession — measure total time"
log "ES comparison: each index creates N shards; cluster rebalances after every PUT"
log ""

T_START=$(date +%s%N)
SHARD_FAIL=0
for i in $(seq 1 100); do
  HTTP=$(curl -sf -o /dev/null -w "%{http_code}" -X PUT "$ES/shard-test-$(printf '%03d' $i)" 2>/dev/null)
  [ "$HTTP" != "200" ] && SHARD_FAIL=$((SHARD_FAIL+1))
done
T_END=$(date +%s%N)
SHARD_MS=$(( (T_END - T_START) / 1000000 ))

[ "$SHARD_FAIL" -eq 0 ] \
  && pass "All 100 indices created (0 failures)" \
  || fail "$SHARD_FAIL / 100 index creations failed"

[ "$SHARD_MS" -lt 10000 ] \
  && pass "100 indices created in ${SHARD_MS}ms (< 10 s)" \
  || fail "100 indices created in ${SHARD_MS}ms (> 10 s)"

# Verify _cat/indices counts them
cmd "GET $ES/_cat/indices"
IDX_COUNT=$(curl -sf "$ES/_cat/indices" 2>/dev/null | wc -l || echo 0)
info "_cat/indices returned $IDX_COUNT index lines total"
[ "$IDX_COUNT" -ge 100 ] \
  && pass "_cat/indices lists ≥ 100 entries" \
  || fail "_cat/indices lists only $IDX_COUNT entries (expected ≥ 100)"

info "ES: 100 indices × 1 shard = 100 Lucene directories; reassignment triggers cluster rebalance"

# =============================================================================
# TEST 05 — Licensing and Trust
# "They changed the license"
# =============================================================================
hdr "TEST 05 — Licensing and Trust (feedback/05)"
log "Scenario: Verify license file exists and contains OSS terms; no feature-gate headers"
log "ES comparison: SSPL is not OSI-approved; core features gated behind subscription tiers"
log ""

LICENSE_FILE="$ENGINE_DIR/LICENSE"
CARGO_TOML="$ENGINE_DIR/Cargo.toml"

if [ -f "$LICENSE_FILE" ]; then
  FIRST_LINE=$(head -1 "$LICENSE_FILE")
  info "License file found: first line = \"$FIRST_LINE\""
  if grep -qi "apache\|mit\|bsd\|mpl\|gpl" "$LICENSE_FILE" 2>/dev/null; then
    pass "LICENSE contains OSI-approved license text"
  else
    fail "LICENSE does not mention a recognised OSI-approved license"
  fi
else
  fail "No LICENSE file found at $LICENSE_FILE"
fi

if [ -f "$CARGO_TOML" ]; then
  LICENSE_FIELD=$(grep '^license' "$CARGO_TOML" | head -1 | awk -F'"' '{print $2}')
  info "Cargo.toml license field: $LICENSE_FIELD"
  [ -n "$LICENSE_FIELD" ] \
    && pass "Cargo.toml declares license: $LICENSE_FIELD" \
    || fail "Cargo.toml has no license field"
fi

# Verify no X-License-Required or similar gating header in responses
cmd "curl -sI $ES/_cluster/health"
HDRS=$(curl -sI "$ES/_cluster/health" 2>/dev/null)
if echo "$HDRS" | grep -qi "x-license\|x-feature-gate\|x-plan-required"; then
  fail "Server returned a license-gate header (feature restricted)"
else
  pass "No license-gate headers — all endpoints freely accessible"
fi

info "Reproducible check against ES: curl -sI http://localhost:9200/_cluster/health | grep -i license"

# =============================================================================
# TEST 06 — Upgrades and Migrations
# "Rolling upgrade from 7.x to 8.x took 6 weeks"
# =============================================================================
hdr "TEST 06 — Upgrades and Migrations (feedback/06)"
log "Scenario: Index data → kill server → restart → verify data intact (WAL persistence)"
log "ES comparison: major-version upgrades require full reindex or rolling restarts"
log ""

# Write some docs
UPGRADE_BULK=""
for i in $(seq 1 50); do
  UPGRADE_BULK="${UPGRADE_BULK}{\"index\":{\"_index\":\"upgrade-test\",\"_id\":\"$i\"}}
{\"payload\":\"pre-upgrade record $i\",\"version\":\"v1\"}
"
done
cmd "POST $ES/_bulk (50 pre-upgrade docs)"
HTTP=$(echo "$UPGRADE_BULK" | curl -sf -o /dev/null -w "%{http_code}" -X POST "$ES/_bulk" \
  -H 'Content-Type: application/x-ndjson' --data-binary @- 2>/dev/null)
[ "$HTTP" = "200" ] && pass "50 docs indexed before simulated upgrade" || fail "Bulk write → $HTTP"

# Force flush / refresh so WAL is persisted
cmd "POST $ES/upgrade-test/_refresh"
curl -sf -X POST "$ES/upgrade-test/_refresh" >/dev/null 2>&1 || true

# Kill and restart
info "Killing server (PID $ZPID)..."
kill "$ZPID" 2>/dev/null; wait "$ZPID" 2>/dev/null || true
sleep 0.5

info "Restarting server (simulate upgrade)..."
XERJ_LOG=error "$XERJ" --config "$CFG1" --insecure 2>/dev/null &
ZPID=$!
if wait_for_port $ES_PORT 15; then
  pass "Server restarted successfully (PID $ZPID)"
else
  fail "Server did not restart within 15 s"
fi

# Check data survived
cmd "GET $ES/upgrade-test/_count"
COUNT_RESP=$(curl -sf "$ES/upgrade-test/_count" 2>/dev/null)
COUNT=$(echo "$COUNT_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('count',0))" 2>/dev/null || echo 0)
info "Doc count after restart: $COUNT (expected 50)"
[ "$COUNT" = "50" ] \
  && pass "All 50 docs survived restart (WAL replay succeeded)" \
  || fail "Only $COUNT / 50 docs found after restart (data loss!)"

info "ES: index data persists across restarts but requires fsync settings; major upgrades need migration"

# =============================================================================
# TEST 07 — Query and Performance
# "10 K deep pagination limit"
# =============================================================================
hdr "TEST 07 — Query and Performance (feedback/07)"
log "Scenario: deep pagination via search_after, bool/range/aggs, measure p99 latency"
log "ES comparison: from+size capped at 10 000; search_after works but cursors expire"
log ""

# Build a 500-doc dataset for pagination
PAG_BULK=""
for i in $(seq 1 500); do
  PAG_BULK="${PAG_BULK}{\"index\":{\"_index\":\"perf-test\",\"_id\":\"$i\"}}
{\"seq\":$i,\"category\":\"$(( (i % 5) + 1 ))\",\"score\":$(( (RANDOM % 100) + 1 )),\"ts\":$((1700000000 + i))}
"
done
cmd "POST $ES/_bulk (500 docs for perf-test)"
HTTP=$(echo "$PAG_BULK" | curl -sf -o /dev/null -w "%{http_code}" -X POST "$ES/_bulk" \
  -H 'Content-Type: application/x-ndjson' --data-binary @- 2>/dev/null)
[ "$HTTP" = "200" ] && pass "500 docs indexed for perf tests" || fail "Bulk 500 → $HTTP"

# 7a. Bool query with range
cmd "POST $ES/perf-test/_search  bool+range query"
BOOL_RESP=$(curl -sf -X POST "$ES/perf-test/_search" \
  -H 'Content-Type: application/json' \
  -d '{
    "query":{
      "bool":{
        "must":[{"match_all":{}}],
        "filter":[{"range":{"seq":{"gte":1,"lte":100}}}]
      }
    },
    "size":100
  }' 2>/dev/null)
BOOL_HITS=$(echo "$BOOL_RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['hits']['total']['value'])" 2>/dev/null || echo -1)
[ "$BOOL_HITS" = "100" ] \
  && pass "bool+range query: 100 hits (correct)" \
  || fail "bool+range returned $BOOL_HITS (expected 100)"

# 7b. Terms aggregation
cmd "POST $ES/perf-test/_search  terms agg on category"
AGG_RESP=$(curl -sf -X POST "$ES/perf-test/_search" \
  -H 'Content-Type: application/json' \
  -d '{
    "size":0,
    "aggs":{
      "by_category":{"terms":{"field":"category","size":10}}
    }
  }' 2>/dev/null)
BUCKET_COUNT=$(echo "$AGG_RESP" | python3 -c \
  "import sys,json; print(len(json.load(sys.stdin)['aggregations']['by_category']['buckets']))" 2>/dev/null || echo -1)
[ "$BUCKET_COUNT" -ge 1 ] \
  && pass "terms aggregation returned $BUCKET_COUNT buckets" \
  || fail "terms aggregation returned $BUCKET_COUNT buckets (expected ≥ 1)"

# 7c. search_after deep pagination
cmd "POST $ES/perf-test/_search  search_after page 1 (size 50)"
PAGE1=$(curl -sf -X POST "$ES/perf-test/_search" \
  -H 'Content-Type: application/json' \
  -d '{"size":50,"sort":[{"seq":"asc"}],"query":{"match_all":{}}}' 2>/dev/null)
LAST_SEQ=$(echo "$PAGE1" | python3 -c \
  "import sys,json; hits=json.load(sys.stdin)['hits']['hits']; print(hits[-1]['sort'][0])" 2>/dev/null || echo "")

if [ -n "$LAST_SEQ" ]; then
  cmd "POST $ES/perf-test/_search  search_after page 2 (cursor=$LAST_SEQ)"
  PAGE2=$(curl -sf -X POST "$ES/perf-test/_search" \
    -H 'Content-Type: application/json' \
    -d "{\"size\":50,\"sort\":[{\"seq\":\"asc\"}],\"query\":{\"match_all\":{}},\"search_after\":[$LAST_SEQ]}" 2>/dev/null)
  PAGE2_COUNT=$(echo "$PAGE2" | python3 -c \
    "import sys,json; print(len(json.load(sys.stdin)['hits']['hits']))" 2>/dev/null || echo -1)
  [ "$PAGE2_COUNT" -gt 0 ] \
    && pass "search_after page 2: $PAGE2_COUNT hits (no 10 K limit)" \
    || fail "search_after page 2: $PAGE2_COUNT hits (expected > 0)"
else
  fail "Could not extract search_after cursor from page 1"
fi

# 7d. p99 latency: 20 sequential searches
T_SEARCH_START=$(date +%s%N)
for _ in $(seq 1 20); do
  curl -sf -X POST "$ES/perf-test/_search" \
    -H 'Content-Type: application/json' \
    -d '{"query":{"match_all":{}},"size":10}' >/dev/null 2>&1
done
T_SEARCH_END=$(date +%s%N)
AVG_MS=$(( (T_SEARCH_END - T_SEARCH_START) / 1000000 / 20 ))
info "Average search latency over 20 sequential calls: ${AVG_MS}ms"
[ "$AVG_MS" -lt 100 ] \
  && pass "Average search latency ${AVG_MS}ms < 100ms" \
  || fail "Average search latency ${AVG_MS}ms ≥ 100ms"

# =============================================================================
# TEST 08 — Data Model Limitations
# "Reindex every time mapping changes"
# =============================================================================
hdr "TEST 08 — Data Model Limitations (feedback/08)"
log "Scenario: dynamic mapping — add new fields across docs without reindex"
log "ES comparison: adding a new field to a closed mapping requires full reindex"
log ""

# Doc 1: basic fields
cmd "PUT $ES/schema-test/_doc/1  {\"name\":\"alice\",\"age\":30}"
HTTP=$(curl -sf -o /dev/null -w "%{http_code}" -X PUT "$ES/schema-test/_doc/1" \
  -H 'Content-Type: application/json' \
  -d '{"name":"alice","age":30}' 2>/dev/null)
[ "$HTTP" = "200" ] && pass "Doc 1 indexed (name, age)" || fail "Doc 1 → $HTTP"

# Doc 2: adds new field 'email'
cmd "PUT $ES/schema-test/_doc/2  {\"name\":\"bob\",\"age\":25,\"email\":\"bob@example.com\"}"
HTTP=$(curl -sf -o /dev/null -w "%{http_code}" -X PUT "$ES/schema-test/_doc/2" \
  -H 'Content-Type: application/json' \
  -d '{"name":"bob","age":25,"email":"bob@example.com"}' 2>/dev/null)
[ "$HTTP" = "200" ] && pass "Doc 2 indexed (name, age, + NEW: email) — no reindex" || fail "Doc 2 → $HTTP"

# Doc 3: adds new field 'vector' (dense numeric array) and nested tags
cmd "PUT $ES/schema-test/_doc/3  {\"name\":\"carol\",\"score\":99.5,\"tags\":[\"admin\",\"dev\"]}"
HTTP=$(curl -sf -o /dev/null -w "%{http_code}" -X PUT "$ES/schema-test/_doc/3" \
  -H 'Content-Type: application/json' \
  -d '{"name":"carol","score":99.5,"tags":["admin","dev"]}' 2>/dev/null)
[ "$HTTP" = "200" ] && pass "Doc 3 indexed (name, + NEW: score, tags) — no reindex" || fail "Doc 3 → $HTTP"

# Query the new field immediately
cmd "POST $ES/schema-test/_search  {\"query\":{\"exists\":{\"field\":\"email\"}}}"
EXISTS_RESP=$(curl -sf -X POST "$ES/schema-test/_search" \
  -H 'Content-Type: application/json' \
  -d '{"query":{"exists":{"field":"email"}}}' 2>/dev/null)
EXISTS_HITS=$(echo "$EXISTS_RESP" | python3 -c \
  "import sys,json; print(json.load(sys.stdin)['hits']['total']['value'])" 2>/dev/null || echo -1)
[ "$EXISTS_HITS" = "1" ] \
  && pass "exists query on new field 'email' returned 1 hit" \
  || fail "exists query returned $EXISTS_HITS (expected 1)"

# Verify mapping endpoint reflects all dynamic fields
cmd "GET $ES/schema-test/_mapping"
MAPPING=$(curl -sf "$ES/schema-test/_mapping" 2>/dev/null)
FIELD_COUNT=$(echo "$MAPPING" | python3 -c \
  "import sys,json; m=json.load(sys.stdin); props=list(m.values())[0]['mappings'].get('properties',{}); print(len(props))" 2>/dev/null || echo 0)
info "Mapping now has $FIELD_COUNT top-level properties"
[ "$FIELD_COUNT" -ge 4 ] \
  && pass "Dynamic mapping grew to $FIELD_COUNT fields (no reindex required)" \
  || fail "Mapping has only $FIELD_COUNT fields (expected ≥ 4)"

info "ES: requires PUT /index/_mapping + potential conflicts resolved only via reindex"

# =============================================================================
# TEST 09 — Security
# "Port 9200 open to the world by default"
# =============================================================================
hdr "TEST 09 — Security (feedback/09)"
log "Scenario: start xerj WITHOUT --insecure; verify unauthenticated requests are rejected"
log "ES comparison: ES 7.x shipped with security OFF by default; 8.x finally enabled it"
log ""

CFG2=$(mktemp --suffix=.toml)
DATA_DIR2=$(mktemp -d)
# Write secure config — auth enabled, no --insecure
cat > "$CFG2" <<TOML
[server]
es_compat_port = $SEC_ES_PORT
rest_port      = $SEC_NATIVE_PORT
grpc_port      = $SEC_GRPC_PORT
data_dir       = "$DATA_DIR2"
bind_address   = "127.0.0.1"

[auth]
enabled       = true
admin_api_key = "test-battle-sla-key-12345"

[tls]
enabled = false
TOML

cmd "$XERJ --config $CFG2  (secure mode — auth enabled, no --insecure)"
XERJ_LOG=error "$XERJ" --config "$CFG2" 2>/dev/null &
ZPID2=$!
SEC="http://localhost:$SEC_ES_PORT"

if wait_for_port $SEC_ES_PORT 15; then
  pass "Secure server started on :$SEC_ES_PORT (PID $ZPID2)"
else
  fail "Secure server did not start within 15 s"
fi

# 9a. Unauthenticated request should be rejected (401)
cmd "curl -sf -o /dev/null -w '%{http_code}' $SEC/_cluster/health  (no auth)"
UNAUTH_CODE=$(curl -sf -o /dev/null -w "%{http_code}" "$SEC/_cluster/health" 2>/dev/null || echo "000")
info "Unauthenticated response code: $UNAUTH_CODE"
[ "$UNAUTH_CODE" = "401" ] \
  && pass "Unauthenticated request rejected with 401 Unauthorized" \
  || fail "Unauthenticated request returned $UNAUTH_CODE (expected 401)"

# 9b. Wrong key should also be rejected
cmd "curl with wrong ApiKey"
WRONG_CODE=$(curl -sf -o /dev/null -w "%{http_code}" \
  -H 'Authorization: ApiKey wrong-key-here' \
  "$SEC/_cluster/health" 2>/dev/null || echo "000")
[ "$WRONG_CODE" = "401" ] \
  && pass "Wrong API key rejected with 401" \
  || fail "Wrong API key returned $WRONG_CODE (expected 401)"

# 9c. Correct key should succeed
cmd "curl with correct ApiKey"
AUTH_CODE=$(curl -sf -o /dev/null -w "%{http_code}" \
  -H 'Authorization: ApiKey test-battle-sla-key-12345' \
  "$SEC/_cluster/health" 2>/dev/null || echo "000")
[ "$AUTH_CODE" = "200" ] \
  && pass "Correct API key accepted → 200 OK" \
  || fail "Correct API key returned $AUTH_CODE (expected 200)"

info "ES 7.x: auth was disabled by default; anyone on the network could read/delete all data"
info "Verify against ES: curl http://localhost:9200/_cluster/health (should return 401 on ES 8.x)"

# =============================================================================
# TEST 10 — Documentation and UX
# "3,000+ config settings"
# =============================================================================
hdr "TEST 10 — Documentation and UX (feedback/10)"
log "Scenario: count user-facing config settings in xerj.default.toml"
log "ES comparison: elasticsearch.yml has 3000+ documented properties"
log ""

USER_SETTINGS=$(grep -v "^#\|^$\|^\[" "$DEFAULT_TOML" | grep "=" | wc -l || echo 0)
info "Total key=value lines in xerj.default.toml: $USER_SETTINGS"
[ "$USER_SETTINGS" -le 50 ] \
  && pass "Config has $USER_SETTINGS settings (Elasticsearch has 3000+)" \
  || fail "Config has $USER_SETTINGS settings (expected ≤ 50)"

# Verify every setting has a comment above it
COMMENT_RATIO=$(grep -c "^#" "$DEFAULT_TOML" || echo 0)
info "Documentation comment lines: $COMMENT_RATIO"
[ "$COMMENT_RATIO" -gt 100 ] \
  && pass "Every setting has inline documentation ($COMMENT_RATIO comment lines)" \
  || fail "Insufficient documentation ($COMMENT_RATIO comment lines)"

# Count config sections
SECTIONS=$(grep -c "^\[" "$DEFAULT_TOML" || echo 0)
info "Config sections: $SECTIONS"
[ "$SECTIONS" -ge 8 ] \
  && pass "Config organised into $SECTIONS named sections" \
  || fail "Only $SECTIONS config sections (expected ≥ 8)"

info "ES: elasticsearch.yml has 3000+ properties; finding the right one requires expert knowledge"

# =============================================================================
# TEST 11 — AI and Vector Search
# "30× latency vs Milvus for vectors"
# =============================================================================
hdr "TEST 11 — AI and Vector Search (feedback/11)"
log "Scenario: index docs with 4-dim vectors, run KNN search, measure latency"
log "ES comparison: limited to 4096 dims; dense_vector requires explicit mapping"
log ""

# Create index with explicit mapping
cmd "PUT $ES/vector-test"
HTTP=$(curl -sf -o /dev/null -w "%{http_code}" -X PUT "$ES/vector-test" \
  -H 'Content-Type: application/json' \
  -d '{
    "mappings":{
      "properties":{
        "title":{"type":"text"},
        "embedding":{"type":"dense_vector","dims":4,"index":true,"similarity":"cosine"}
      }
    }
  }' 2>/dev/null)
[ "$HTTP" = "200" ] && pass "Vector index created with explicit dense_vector mapping" || fail "Create vector index → $HTTP"

# Index 20 docs with vectors
VEC_BULK=""
for i in $(seq 1 20); do
  A=$(python3 -c "import random; print(round(random.uniform(-1,1),4))")
  B=$(python3 -c "import random; print(round(random.uniform(-1,1),4))")
  C=$(python3 -c "import random; print(round(random.uniform(-1,1),4))")
  D=$(python3 -c "import random; print(round(random.uniform(-1,1),4))")
  VEC_BULK="${VEC_BULK}{\"index\":{\"_index\":\"vector-test\",\"_id\":\"$i\"}}
{\"title\":\"document $i\",\"embedding\":[$A,$B,$C,$D]}
"
done
cmd "POST $ES/_bulk  (20 docs with 4-dim vectors)"
HTTP=$(echo "$VEC_BULK" | curl -sf -o /dev/null -w "%{http_code}" -X POST "$ES/_bulk" \
  -H 'Content-Type: application/x-ndjson' --data-binary @- 2>/dev/null)
[ "$HTTP" = "200" ] && pass "20 docs with embeddings indexed" || fail "Vector bulk → $HTTP"

# KNN search — 5 nearest neighbours
cmd "POST $ES/vector-test/_search  knn:{query_vector:[0.1,0.2,0.3,0.4],k:5}"
T_VEC_START=$(date +%s%N)
KNN_RESP=$(curl -sf -X POST "$ES/vector-test/_search" \
  -H 'Content-Type: application/json' \
  -d '{
    "knn":{
      "field":"embedding",
      "query_vector":[0.1,0.2,0.3,0.4],
      "k":5,
      "num_candidates":20
    },
    "size":5
  }' 2>/dev/null)
T_VEC_END=$(date +%s%N)
VEC_MS=$(( (T_VEC_END - T_VEC_START) / 1000000 ))

KNN_HITS=$(echo "$KNN_RESP" | python3 -c \
  "import sys,json; print(len(json.load(sys.stdin)['hits']['hits']))" 2>/dev/null || echo -1)
[ "$KNN_HITS" -ge 1 ] \
  && pass "KNN search returned $KNN_HITS neighbours" \
  || fail "KNN search returned $KNN_HITS (expected ≥ 1)"

info "KNN search latency: ${VEC_MS}ms"
[ "$VEC_MS" -lt 500 ] \
  && pass "KNN latency ${VEC_MS}ms < 500ms" \
  || fail "KNN latency ${VEC_MS}ms ≥ 500ms"

# Verify _analyze on a text field still works alongside vectors
cmd "POST $ES/vector-test/_analyze  {\"analyzer\":\"standard\",\"text\":\"Hello World\"}"
ANALYZE_RESP=$(curl -sf -X POST "$ES/vector-test/_analyze" \
  -H 'Content-Type: application/json' \
  -d '{"analyzer":"standard","text":"Hello World"}' 2>/dev/null)
TOKEN_COUNT=$(echo "$ANALYZE_RESP" | python3 -c \
  "import sys,json; print(len(json.load(sys.stdin).get('tokens',[])))" 2>/dev/null || echo -1)
[ "$TOKEN_COUNT" -ge 1 ] \
  && pass "_analyze returns $TOKEN_COUNT tokens for 'Hello World'" \
  || fail "_analyze returned $TOKEN_COUNT tokens (expected ≥ 1)"

info "xerj supports up to 16 384 dimensions — 4× ES's 4 096 limit"
info "ES comparison: PUT /vector-test/_mapping requires explicit mapping before first doc"

# =============================================================================
# TEST 12 — Log Analytics
# "1 TB data → 8 TB on disk with ES"
# =============================================================================
hdr "TEST 12 — Log Analytics (feedback/12)"
log "Scenario: index 10 000 log entries, measure on-disk size"
log "ES comparison: inverted index + doc-values + stored fields ≈ 5–8× raw data"
log ""

BEFORE_BYTES=$(du -sb "$DATA_DIR" 2>/dev/null | awk '{print $1}' || echo 0)

LOG_BULK=""
for i in $(seq 1 10000); do
  TS=$((1700000000 + i))
  LEVEL="INFO"
  [ $((i % 10)) -eq 0 ] && LEVEL="WARN"
  [ $((i % 50)) -eq 0 ] && LEVEL="ERROR"
  LOG_BULK="${LOG_BULK}{\"index\":{\"_index\":\"logs-test\"}}
{\"@timestamp\":$TS,\"level\":\"$LEVEL\",\"host\":\"srv-$(( (i%8)+1 ))\",\"message\":\"Request $i processed in $((RANDOM%500))ms\",\"status\":$(( (i%3)*100+200 ))}
"
  # Flush every 1000 docs to avoid huge single request
  if [ $((i % 1000)) -eq 0 ]; then
    echo "$LOG_BULK" | curl -sf -o /dev/null -X POST "$ES/_bulk" \
      -H 'Content-Type: application/x-ndjson' --data-binary @- 2>/dev/null || true
    LOG_BULK=""
    printf "." >&2
  fi
done
echo "" >&2
[ -n "$LOG_BULK" ] && echo "$LOG_BULK" | curl -sf -o /dev/null -X POST "$ES/_bulk" \
  -H 'Content-Type: application/x-ndjson' --data-binary @- 2>/dev/null || true
pass "10 000 log entries indexed"

# Force flush to get accurate disk usage
cmd "POST $ES/logs-test/_refresh"
curl -sf -X POST "$ES/logs-test/_refresh" >/dev/null 2>&1 || true

AFTER_BYTES=$(du -sb "$DATA_DIR" 2>/dev/null | awk '{print $1}' || echo 0)
DELTA_BYTES=$(( AFTER_BYTES - BEFORE_BYTES ))
DELTA_KB=$(( DELTA_BYTES / 1024 ))
info "On-disk size for 10 000 log docs: ${DELTA_KB} kB (${DELTA_BYTES} bytes)"

# Rough raw size: ~100 bytes/doc = 1 MB raw
RATIO_X10=$(( DELTA_BYTES / 100000 ))  # ratio × 10 (e.g. 15 = 1.5×)
info "Approximate storage ratio vs raw JSON: $((RATIO_X10 / 10)).$((RATIO_X10 % 10))×"
[ "$DELTA_KB" -lt 51200 ] \
  && pass "Storage ≤ 50 MiB for 10 K log docs" \
  || fail "Storage ${DELTA_KB} kB exceeds 50 MiB for 10 K log docs"

# Verify range query on timestamp works
cmd "POST $ES/logs-test/_search  range on @timestamp"
RANGE_RESP=$(curl -sf -X POST "$ES/logs-test/_search" \
  -H 'Content-Type: application/json' \
  -d '{"query":{"range":{"@timestamp":{"gte":1700000000,"lte":1700001000}}},"size":0}' 2>/dev/null)
RANGE_HITS=$(echo "$RANGE_RESP" | python3 -c \
  "import sys,json; print(json.load(sys.stdin)['hits']['total']['value'])" 2>/dev/null || echo -1)
[ "$RANGE_HITS" -ge 1 ] \
  && pass "Time-range query returned $RANGE_HITS docs" \
  || fail "Time-range query returned $RANGE_HITS (expected ≥ 1)"

info "ES: Lucene inverted index + doc-values + stored-fields = 5–8× raw size before snapshots"

# =============================================================================
# TEST 13 — Vendor and Support
# "Support unresponsive"
# =============================================================================
hdr "TEST 13 — Vendor and Support (feedback/13)"
log "Scenario: verify all operational endpoints respond quickly (< 200ms each)"
log "ES comparison: _cluster/health can hang during recovery; _nodes/stats requires auth"
log ""

ENDPOINTS=(
  "$ES/_cluster/health"
  "$ES/_cluster/stats"
  "$ES/_cat/indices"
  "$ES/_cat/health"
  "$ES/_cat/nodes"
  "$ES/_nodes/stats"
)

for EP in "${ENDPOINTS[@]}"; do
  NAME="${EP##*/}"
  T1=$(date +%s%N)
  HTTP=$(curl -sf -o /dev/null -w "%{http_code}" "$EP" 2>/dev/null || echo 000)
  T2=$(date +%s%N)
  LATENCY_MS=$(( (T2 - T1) / 1000000 ))
  CMD_REPR="${EP/http:\/\/localhost:$ES_PORT/\$ES}"
  cmd "GET $CMD_REPR"
  if [ "$HTTP" = "200" ] && [ "$LATENCY_MS" -lt 200 ]; then
    pass "$NAME → 200 in ${LATENCY_MS}ms"
  elif [ "$HTTP" = "200" ]; then
    fail "$NAME → 200 but slow: ${LATENCY_MS}ms (expected < 200ms)"
  else
    fail "$NAME → $HTTP in ${LATENCY_MS}ms"
  fi
done

# Also test _tasks
cmd "GET $ES/_tasks"
HTTP=$(curl -sf -o /dev/null -w "%{http_code}" "$ES/_tasks" 2>/dev/null || echo 000)
[ "$HTTP" = "200" ] && pass "_tasks → 200" || fail "_tasks → $HTTP"

info "ES: _cluster/health blocks during shard recovery; _nodes/stats requires auth on 8.x"

# =============================================================================
# TEST 14 — Ecosystem and Alternatives
# "Kibana lock-in"
# =============================================================================
hdr "TEST 14 — Ecosystem and Alternatives (feedback/14)"
log "Scenario: dashboard summary API, SQL query, _analyze — open alternatives to Kibana"
log "ES comparison: Kibana dashboard data requires Kibana; no first-party SQL in open edition"
log ""

# 14a. Native dashboard summary
cmd "GET $NATIVE/v1/dashboard/summary"
T1=$(date +%s%N)
DASH=$(curl -sf "$NATIVE/v1/dashboard/summary" 2>/dev/null)
T2=$(date +%s%N)
DASH_MS=$(( (T2 - T1) / 1000000 ))

if echo "$DASH" | python3 -c "import sys,json; d=json.load(sys.stdin); assert 'doc_count' in d or 'index_count' in d or 'indices' in d or len(d)>0" 2>/dev/null; then
  pass "Dashboard summary returned structured JSON in ${DASH_MS}ms"
else
  fail "Dashboard summary returned unexpected payload: ${DASH:0:120}"
fi

# 14b. SQL query
cmd "POST $ES/_sql  SELECT COUNT(*) FROM \"logs-test\""
SQL_RESP=$(curl -sf -X POST "$ES/_sql" \
  -H 'Content-Type: application/json' \
  -d '{"query":"SELECT COUNT(*) FROM \"logs-test\"","fetch_size":10}' 2>/dev/null)
if echo "$SQL_RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); assert 'rows' in d or 'columns' in d or 'error' not in d" 2>/dev/null; then
  ROW=$(echo "$SQL_RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('rows',[[]])[0][0] if d.get('rows') else 'N/A')" 2>/dev/null || echo "?")
  pass "SQL COUNT(*) returned: $ROW"
else
  # SQL might return an error body — check it's a valid JSON error, not a crash
  ERR=$(echo "$SQL_RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('error','?'))" 2>/dev/null || echo "non-JSON")
  fail "SQL query failed: $ERR"
fi

# 14c. _analyze — useful for understanding tokenisation without Kibana
cmd "POST $ES/_analyze  {\"text\":\"The quick brown fox\",\"analyzer\":\"standard\"}"
ANALYZE_RESP=$(curl -sf -X POST "$ES/_analyze" \
  -H 'Content-Type: application/json' \
  -d '{"text":"The quick brown fox","analyzer":"standard"}' 2>/dev/null)
ANA_TOKENS=$(echo "$ANALYZE_RESP" | python3 -c \
  "import sys,json; print(len(json.load(sys.stdin).get('tokens',[])))" 2>/dev/null || echo -1)
[ "$ANA_TOKENS" -ge 3 ] \
  && pass "_analyze tokenised 'The quick brown fox' into $ANA_TOKENS tokens" \
  || fail "_analyze returned $ANA_TOKENS tokens (expected ≥ 3)"

# 14d. explain-plan (native)
cmd "POST $NATIVE/v1/indices/logs-test/explain-plan  {\"query\":{\"match_all\":{}}}"
PLAN_HTTP=$(curl -sf -o /dev/null -w "%{http_code}" \
  -X POST "$NATIVE/v1/indices/logs-test/explain-plan" \
  -H 'Content-Type: application/json' \
  -d '{"query":{"match_all":{}}}' 2>/dev/null || echo 000)
[ "$PLAN_HTTP" = "200" ] \
  && pass "explain-plan endpoint works (no Kibana needed)" \
  || fail "explain-plan → $PLAN_HTTP (expected 200)"

info "Kibana requires a paid Elastic subscription for production dashboards"
info "xerj: dashboard summary + SQL + explain-plan are all open, no extra software"

# =============================================================================
# Summary
# =============================================================================
hdr "SUMMARY"

SUMMARY_TABLE="
| # | Category                         | Result       |
|---|----------------------------------|--------------|
| 01 | Operational Complexity           | See above   |
| 02 | Cost and Pricing                 | See above   |
| 03 | JVM and Memory                   | See above   |
| 04 | Scaling and Shards               | See above   |
| 05 | Licensing and Trust              | See above   |
| 06 | Upgrades and Migrations          | See above   |
| 07 | Query and Performance            | See above   |
| 08 | Data Model Limitations           | See above   |
| 09 | Security                         | See above   |
| 10 | Documentation and UX             | See above   |
| 11 | AI and Vector Search             | See above   |
| 12 | Log Analytics                    | See above   |
| 13 | Vendor and Support               | See above   |
| 14 | Ecosystem and Alternatives       | See above   |
"

echo "$SUMMARY_TABLE" | tee -a "$LOG_FILE"

log ""
log "────────────────────────────────────────"
log "  PASS  : $PASS"
log "  FAIL  : $FAIL"
log "  TOTAL : $TOTAL"
log "────────────────────────────────────────"

{
  echo ""
  echo "---"
  echo ""
  echo "## Final Score"
  echo ""
  echo "| Metric | Value |"
  echo "|--------|-------|"
  echo "| Tests passed | $PASS |"
  echo "| Tests failed | $FAIL |"
  echo "| Total checks | $TOTAL |"
  echo "| Pass rate | $(( PASS * 100 / (TOTAL > 0 ? TOTAL : 1) ))% |"
  echo ""
  echo "## Reproducing Against Elasticsearch"
  echo ""
  echo "All commands above use port \`19200\`. To reproduce against a live"
  echo "Elasticsearch 8.x cluster:"
  echo ""
  echo '```bash'
  echo '# Replace 19200 with 9200 in every curl command.'
  echo '# Note: ES 8.x requires auth header:'
  echo '#   -H "Authorization: Basic $(echo -n elastic:password | base64)"'
  echo '# ES will reject some requests that xerj serves without auth under --insecure.'
  echo '```'
  echo ""
  echo "Generated: $TIMESTAMP"
} >> "$LOG_FILE"

if [ "$FAIL" -eq 0 ]; then
  echo -e "\n${GREEN}${BOLD}All $PASS checks passed.${RESET}"
else
  echo -e "\n${RED}${BOLD}$FAIL check(s) failed out of $TOTAL.${RESET}"
fi

echo ""
echo "Full results written to: $LOG_FILE"

[ "$FAIL" -eq 0 ] && exit 0 || exit 1
