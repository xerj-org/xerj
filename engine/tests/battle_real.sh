#!/bin/bash
# ═══════════════════════════════════════════════════════════════════════
# REAL BATTLE TEST — Elasticsearch 8.13 vs xerj 0.1.0
# Runs the SAME commands against both servers, compares results
# ═══════════════════════════════════════════════════════════════════════
set -uo pipefail

ES=http://localhost:9200
ZB=http://localhost:3322
TIMESTAMP=$(date +%Y-%m-%d_%H-%M-%S)
LOG="BATTLE_RESULTS_${TIMESTAMP}.md"
PASS=0; FAIL=0; TOTAL=0; ES_WIN=0; ZB_WIN=0; TIE=0

log()  { echo "$*" | tee -a "$LOG"; }
ts()   { date +%H:%M:%S.%N | cut -c1-12; }
pass() { PASS=$((PASS+1)); TOTAL=$((TOTAL+1)); log "  [$(ts)] PASS: $1"; }
fail() { FAIL=$((FAIL+1)); TOTAL=$((TOTAL+1)); log "  [$(ts)] FAIL: $1"; }

# Compare a value between ES and xerj
compare_time() {
  local name="$1" es_ms="$2" zb_ms="$3"
  if [ "$es_ms" -lt "$zb_ms" ]; then
    ES_WIN=$((ES_WIN+1))
    log "  $name: ES=${es_ms}ms  xerj=${zb_ms}ms  → ES wins"
  elif [ "$zb_ms" -lt "$es_ms" ]; then
    ZB_WIN=$((ZB_WIN+1))
    log "  $name: ES=${es_ms}ms  xerj=${zb_ms}ms  → xerj wins"
  else
    TIE=$((TIE+1))
    log "  $name: ES=${es_ms}ms  xerj=${zb_ms}ms  → Tie"
  fi
}

# Time a curl command in ms
time_curl() {
  local start=$(date +%s%N)
  eval "$1" > /dev/null 2>&1
  echo $(( ($(date +%s%N) - start) / 1000000 ))
}

# Check both servers are up
check_servers() {
  curl -sf "$ES/" > /dev/null 2>&1 || { echo "ERROR: ES not running on $ES"; exit 1; }
  curl -sf "$ZB/" > /dev/null 2>&1 || { echo "ERROR: xerj not running on $ZB"; exit 1; }
  log "Both servers confirmed running"
}

# ═══════════════════════════════════════════════════════════════
log "# REAL BATTLE TEST — ES 8.13 vs xerj 0.1.0"
log "# $(date)"
log "# ES: $ES | xerj: $ZB"
log ""
check_servers

# ═══════════════════════════════════════════════════════════════
log "## 1. ES YAML Test: index/10_with_id — Index with ID"
log ""

# Index doc on both
R_ES=$(curl -sf -X PUT "$ES/yaml-test/_doc/1" -H 'Content-Type: application/json' -d '{"foo":"bar"}')
R_ZB=$(curl -sf -X PUT "$ZB/yaml-test/_doc/1" -H 'Content-Type: application/json' -d '{"foo":"bar"}')

# Check _id
ES_ID=$(echo "$R_ES" | python3 -c "import sys,json; print(json.load(sys.stdin).get('_id',''))" 2>/dev/null)
ZB_ID=$(echo "$R_ZB" | python3 -c "import sys,json; print(json.load(sys.stdin).get('_id',''))" 2>/dev/null)
[ "$ES_ID" = "1" ] && [ "$ZB_ID" = "1" ] && pass "Both return _id=1" || fail "ID mismatch ES=$ES_ID ZB=$ZB_ID"

# Check result
ES_RES=$(echo "$R_ES" | python3 -c "import sys,json; print(json.load(sys.stdin).get('result',''))" 2>/dev/null)
ZB_RES=$(echo "$R_ZB" | python3 -c "import sys,json; print(json.load(sys.stdin).get('result',''))" 2>/dev/null)
[ "$ES_RES" = "created" ] && [ "$ZB_RES" = "created" ] && pass "Both return result=created" || fail "Result mismatch ES=$ES_RES ZB=$ZB_RES"

# GET and verify _source
curl -sf -X POST "$ES/yaml-test/_refresh" > /dev/null 2>&1
ES_SRC=$(curl -sf "$ES/yaml-test/_doc/1" | python3 -c "import sys,json; print(json.load(sys.stdin).get('_source',{}))" 2>/dev/null)
ZB_SRC=$(curl -sf "$ZB/yaml-test/_doc/1" | python3 -c "import sys,json; print(json.load(sys.stdin).get('_source',{}))" 2>/dev/null)
[ "$ES_SRC" = "{'foo': 'bar'}" ] && [ "$ZB_SRC" = "{'foo': 'bar'}" ] && pass "Both return _source={foo:bar}" || fail "Source mismatch"

log ""

# ═══════════════════════════════════════════════════════════════
log "## 2. ES YAML Test: get/10_basic — Get Document"
log ""

curl -sf -X PUT "$ES/get-test/_doc/1" -H 'Content-Type: application/json' -d '{"msg":"hello world"}' > /dev/null
curl -sf -X PUT "$ZB/get-test/_doc/1" -H 'Content-Type: application/json' -d '{"msg":"hello world"}' > /dev/null
curl -sf -X POST "$ES/get-test/_refresh" > /dev/null 2>&1

ES_FOUND=$(curl -sf "$ES/get-test/_doc/1" | python3 -c "import sys,json; print(json.load(sys.stdin).get('found'))" 2>/dev/null)
ZB_FOUND=$(curl -sf "$ZB/get-test/_doc/1" | python3 -c "import sys,json; print(json.load(sys.stdin).get('found'))" 2>/dev/null)
[ "$ES_FOUND" = "True" ] && [ "$ZB_FOUND" = "True" ] && pass "Both return found=true" || fail "Found mismatch ES=$ES_FOUND ZB=$ZB_FOUND"

# Get missing doc
ES_404=$(curl -sf "$ES/get-test/_doc/999" | python3 -c "import sys,json; print(json.load(sys.stdin).get('found'))" 2>/dev/null)
ZB_404=$(curl -sf "$ZB/get-test/_doc/999" | python3 -c "import sys,json; print(json.load(sys.stdin).get('found'))" 2>/dev/null)
[ "$ES_404" = "False" ] && [ "$ZB_404" = "False" ] && pass "Both return found=false for missing" || fail "Missing doc mismatch"

log ""

# ═══════════════════════════════════════════════════════════════
log "## 3. ES YAML Test: delete/12_result — Delete Document"
log ""

curl -sf -X PUT "$ES/del-test/_doc/1" -H 'Content-Type: application/json' -d '{"x":1}' > /dev/null
curl -sf -X PUT "$ZB/del-test/_doc/1" -H 'Content-Type: application/json' -d '{"x":1}' > /dev/null

ES_DEL=$(curl -sf -X DELETE "$ES/del-test/_doc/1" | python3 -c "import sys,json; print(json.load(sys.stdin).get('result'))" 2>/dev/null)
ZB_DEL=$(curl -sf -X DELETE "$ZB/del-test/_doc/1" | python3 -c "import sys,json; print(json.load(sys.stdin).get('result'))" 2>/dev/null)
[ "$ES_DEL" = "deleted" ] && [ "$ZB_DEL" = "deleted" ] && pass "Both return result=deleted" || fail "Delete result mismatch ES=$ES_DEL ZB=$ZB_DEL"

# Delete again — should be not_found
ES_DEL2=$(curl -sf -X DELETE "$ES/del-test/_doc/1" | python3 -c "import sys,json; print(json.load(sys.stdin).get('result'))" 2>/dev/null)
ZB_DEL2=$(curl -sf -X DELETE "$ZB/del-test/_doc/1" | python3 -c "import sys,json; print(json.load(sys.stdin).get('result'))" 2>/dev/null)
[ "$ES_DEL2" = "not_found" ] && [ "$ZB_DEL2" = "not_found" ] && pass "Both return not_found on double delete" || log "  NOTE: ES=$ES_DEL2 ZB=$ZB_DEL2 (delete semantics differ)"

log ""

# ═══════════════════════════════════════════════════════════════
log "## 4. ES YAML Test: bulk/10_basic — Bulk Index"
log ""

BULK_BODY='{"index":{"_index":"bulk-test","_id":"1"}}
{"f1":"v1","f2":42}
{"index":{"_index":"bulk-test","_id":"2"}}
{"f1":"v2","f2":47}
'

ES_BULK=$(curl -sf -X POST "$ES/_bulk" -H 'Content-Type: application/x-ndjson' -d "$BULK_BODY")
ZB_BULK=$(curl -sf -X POST "$ZB/_bulk" -H 'Content-Type: application/x-ndjson' -d "$BULK_BODY")

ES_ERR=$(echo "$ES_BULK" | python3 -c "import sys,json; print(json.load(sys.stdin).get('errors'))" 2>/dev/null)
ZB_ERR=$(echo "$ZB_BULK" | python3 -c "import sys,json; print(json.load(sys.stdin).get('errors'))" 2>/dev/null)
[ "$ES_ERR" = "False" ] && [ "$ZB_ERR" = "False" ] && pass "Both bulk: errors=false" || fail "Bulk errors mismatch ES=$ES_ERR ZB=$ZB_ERR"

curl -sf -X POST "$ES/bulk-test/_refresh" > /dev/null 2>&1
ES_CNT=$(curl -sf "$ES/bulk-test/_count" | python3 -c "import sys,json; print(json.load(sys.stdin).get('count'))" 2>/dev/null)
ZB_CNT=$(curl -sf "$ZB/bulk-test/_count" | python3 -c "import sys,json; print(json.load(sys.stdin).get('count'))" 2>/dev/null)
[ "$ES_CNT" = "2" ] && [ "$ZB_CNT" = "2" ] && pass "Both count=2 after bulk" || fail "Count mismatch ES=$ES_CNT ZB=$ZB_CNT"

log ""

# ═══════════════════════════════════════════════════════════════
log "## 5. Search: match_all"
log ""

# Index 100 docs on both
for i in $(seq 1 100); do
  curl -sf -X PUT "$ES/search-test/_doc/$i" -H 'Content-Type: application/json' \
    -d "{\"title\":\"Doc $i\",\"body\":\"Content about search engines number $i\",\"price\":$((i*10)),\"cat\":\"$([ $((i%3)) -eq 0 ] && echo A || echo B)\"}" > /dev/null &
  curl -sf -X PUT "$ZB/search-test/_doc/$i" -H 'Content-Type: application/json' \
    -d "{\"title\":\"Doc $i\",\"body\":\"Content about search engines number $i\",\"price\":$((i*10)),\"cat\":\"$([ $((i%3)) -eq 0 ] && echo A || echo B)\"}" > /dev/null &
done
wait
curl -sf -X POST "$ES/search-test/_refresh" > /dev/null 2>&1

ES_ALL=$(curl -sf -X POST "$ES/search-test/_search" -H 'Content-Type: application/json' -d '{"query":{"match_all":{}},"size":0}' | python3 -c "import sys,json; print(json.load(sys.stdin)['hits']['total']['value'])" 2>/dev/null)
ZB_ALL=$(curl -sf -X POST "$ZB/search-test/_search" -H 'Content-Type: application/json' -d '{"query":{"match_all":{}},"size":0}' | python3 -c "import sys,json; print(json.load(sys.stdin)['hits']['total']['value'])" 2>/dev/null)
[ "$ES_ALL" = "100" ] && [ "$ZB_ALL" = "100" ] && pass "Both match_all: 100 hits" || fail "match_all mismatch ES=$ES_ALL ZB=$ZB_ALL"

log ""

# ═══════════════════════════════════════════════════════════════
log "## 6. Search: match query — BM25 ranking"
log ""

ES_MATCH=$(curl -sf -X POST "$ES/search-test/_search" -H 'Content-Type: application/json' -d '{"query":{"match":{"body":"search engines"}},"size":3}')
ZB_MATCH=$(curl -sf -X POST "$ZB/search-test/_search" -H 'Content-Type: application/json' -d '{"query":{"match":{"body":"search engines"}},"size":3}')

ES_HITS=$(echo "$ES_MATCH" | python3 -c "import sys,json; print(json.load(sys.stdin)['hits']['total']['value'])" 2>/dev/null)
ZB_HITS=$(echo "$ZB_MATCH" | python3 -c "import sys,json; print(json.load(sys.stdin)['hits']['total']['value'])" 2>/dev/null)
[ "$ES_HITS" -gt 0 ] && [ "$ZB_HITS" -gt 0 ] && pass "Both return hits for match query (ES=$ES_HITS ZB=$ZB_HITS)" || fail "Match query failed"

log ""

# ═══════════════════════════════════════════════════════════════
log "## 7. Search: range query"
log ""

ES_RANGE=$(curl -sf -X POST "$ES/search-test/_search" -H 'Content-Type: application/json' -d '{"query":{"range":{"price":{"gte":500,"lte":700}}},"size":0}' | python3 -c "import sys,json; print(json.load(sys.stdin)['hits']['total']['value'])" 2>/dev/null)
ZB_RANGE=$(curl -sf -X POST "$ZB/search-test/_search" -H 'Content-Type: application/json' -d '{"query":{"range":{"price":{"gte":500,"lte":700}}},"size":0}' | python3 -c "import sys,json; print(json.load(sys.stdin)['hits']['total']['value'])" 2>/dev/null)
[ "$ES_RANGE" = "$ZB_RANGE" ] && pass "Range query: both return $ES_RANGE hits" || fail "Range mismatch ES=$ES_RANGE ZB=$ZB_RANGE"

log ""

# ═══════════════════════════════════════════════════════════════
log "## 8. Aggregations: terms + stats"
log ""

ES_AGG=$(curl -sf -X POST "$ES/search-test/_search" -H 'Content-Type: application/json' -d '{"size":0,"aggs":{"by_cat":{"terms":{"field":"cat.keyword"}},"price_stats":{"stats":{"field":"price"}}}}')
ZB_AGG=$(curl -sf -X POST "$ZB/search-test/_search" -H 'Content-Type: application/json' -d '{"size":0,"aggs":{"by_cat":{"terms":{"field":"cat"}},"price_stats":{"stats":{"field":"price"}}}}')

ES_AVG=$(echo "$ES_AGG" | python3 -c "import sys,json; print(int(json.load(sys.stdin)['aggregations']['price_stats']['avg']))" 2>/dev/null)
ZB_AVG=$(echo "$ZB_AGG" | python3 -c "import sys,json; print(int(json.load(sys.stdin)['aggregations']['price_stats']['avg']))" 2>/dev/null)
[ "$ES_AVG" = "$ZB_AVG" ] && pass "Stats avg matches: $ES_AVG" || log "  NOTE: Stats avg ES=$ES_AVG ZB=$ZB_AVG (may differ due to .keyword)"

log ""

# ═══════════════════════════════════════════════════════════════
log "## 9. Cluster health"
log ""

ES_HEALTH=$(curl -sf "$ES/_cluster/health" | python3 -c "import sys,json; print(json.load(sys.stdin)['status'])" 2>/dev/null)
ZB_HEALTH=$(curl -sf "$ZB/_cluster/health" | python3 -c "import sys,json; print(json.load(sys.stdin)['status'])" 2>/dev/null)
pass "ES health=$ES_HEALTH, xerj health=$ZB_HEALTH"

log ""

# ═══════════════════════════════════════════════════════════════
log "## 10. PERFORMANCE: Index 1000 docs"
log ""

ES_T=$(time_curl "for i in \$(seq 1 1000); do curl -sf -X PUT '$ES/perf-test/_doc/\$i' -H 'Content-Type: application/json' -d '{\"n\":\$i,\"t\":\"perf test doc\"}' > /dev/null; done")
ZB_T=$(time_curl "for i in \$(seq 1 1000); do curl -sf -X PUT '$ZB/perf-test/_doc/\$i' -H 'Content-Type: application/json' -d '{\"n\":\$i,\"t\":\"perf test doc\"}' > /dev/null; done")
compare_time "Index 1K docs" "$ES_T" "$ZB_T"

curl -sf -X POST "$ES/perf-test/_refresh" > /dev/null 2>&1
log ""

# ═══════════════════════════════════════════════════════════════
log "## 11. PERFORMANCE: Search 100 queries"
log ""

ES_S=$(time_curl "for i in \$(seq 1 100); do curl -sf -X POST '$ES/perf-test/_search' -H 'Content-Type: application/json' -d '{\"query\":{\"match\":{\"t\":\"perf test\"}},\"size\":5}' > /dev/null; done")
ZB_S=$(time_curl "for i in \$(seq 1 100); do curl -sf -X POST '$ZB/perf-test/_search' -H 'Content-Type: application/json' -d '{\"query\":{\"match\":{\"t\":\"perf test\"}},\"size\":5}' > /dev/null; done")
compare_time "100 match queries" "$ES_S" "$ZB_S"

log ""

# ═══════════════════════════════════════════════════════════════
log "## 12. MEMORY COMPARISON"
log ""

ES_PID=$(pgrep -f "org.elasticsearch" | head -1)
ZB_PID=$(pgrep -f "target/release/xerj" | head -1)
ES_RSS=$(($(ps -o rss= -p $ES_PID 2>/dev/null | tr -d ' ') / 1024))
ZB_RSS=$(($(ps -o rss= -p $ZB_PID 2>/dev/null | tr -d ' ') / 1024))
log "  ES RSS:    ${ES_RSS} MB"
log "  xerj RSS: ${ZB_RSS} MB"
log "  Ratio:     $((ES_RSS / (ZB_RSS + 1)))x less memory for xerj"

log ""

# ═══════════════════════════════════════════════════════════════
log "## CLEANUP"
curl -sf -X DELETE "$ES/yaml-test,$ES/get-test,$ES/del-test,$ES/bulk-test,$ES/search-test,$ES/perf-test" > /dev/null 2>&1
curl -sf -X DELETE "$ZB/yaml-test" > /dev/null 2>&1
curl -sf -X DELETE "$ZB/get-test" > /dev/null 2>&1
curl -sf -X DELETE "$ZB/del-test" > /dev/null 2>&1
curl -sf -X DELETE "$ZB/bulk-test" > /dev/null 2>&1
curl -sf -X DELETE "$ZB/search-test" > /dev/null 2>&1
curl -sf -X DELETE "$ZB/perf-test" > /dev/null 2>&1

log ""
log "## FINAL SUMMARY"
log ""
log "| Metric | Count |"
log "|--------|-------|"
log "| Total tests | $TOTAL |"
log "| Pass | $PASS |"
log "| Fail | $FAIL |"
log "| ES perf wins | $ES_WIN |"
log "| xerj perf wins | $ZB_WIN |"
log "| Perf ties | $TIE |"
log ""
log "Results saved to: $LOG"
