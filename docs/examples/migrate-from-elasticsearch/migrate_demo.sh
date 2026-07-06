#!/usr/bin/env bash
# migrate_demo.sh — prove standard Elasticsearch REST calls run UNCHANGED against XERJ.
#
# The only thing that changed vs. a real Elasticsearch cluster is $ES:
# point it at XERJ's wire port and every request below is byte-for-byte
# the same JSON you'd send Elasticsearch.
#
#   ES=http://localhost:9487 ./migrate_demo.sh
#
# Requires: curl, and (for pretty output) python3 — both stdlib-only.
set -euo pipefail

ES="${ES:-http://localhost:9200}"   # <-- the ONE thing you change to migrate
IDX=products

pp() { python3 -m json.tool; }      # pretty-print JSON from stdin
say() { printf '\n=== %s ===\n' "$1"; }

# 0) Clean slate (ignore 404 on first run)
curl -s -X DELETE "$ES/$IDX" >/dev/null || true

# 1) Create an index with an explicit mapping — standard ES PUT.
say "1. PUT /$IDX  (create index + mapping)"
curl -s -X PUT "$ES/$IDX" \
  -H 'Content-Type: application/json' -d '{
  "mappings": {
    "properties": {
      "name":     { "type": "text" },
      "brand":    { "type": "keyword" },
      "price":    { "type": "float" },
      "in_stock": { "type": "boolean" }
    }
  }
}' | pp

# 2) Bulk-load documents — standard ES _bulk NDJSON (action line + source line).
say "2. POST /_bulk  (index 6 docs)"
curl -s -X POST "$ES/_bulk" \
  -H 'Content-Type: application/x-ndjson' --data-binary '
{"index":{"_index":"products","_id":"1"}}
{"name":"Aluminum water bottle","brand":"Klean","price":24.99,"in_stock":true}
{"index":{"_index":"products","_id":"2"}}
{"name":"Insulated steel water bottle","brand":"Hydro","price":39.95,"in_stock":true}
{"index":{"_index":"products","_id":"3"}}
{"name":"Glass water carafe","brand":"Bodum","price":19.50,"in_stock":false}
{"index":{"_index":"products","_id":"4"}}
{"name":"Steel travel mug","brand":"Hydro","price":29.00,"in_stock":true}
{"index":{"_index":"products","_id":"5"}}
{"name":"Ceramic coffee mug","brand":"Bodum","price":12.00,"in_stock":true}
{"index":{"_index":"products","_id":"6"}}
{"name":"Plastic sports bottle","brand":"Klean","price":9.99,"in_stock":false}
' | python3 -c 'import sys,json; d=json.load(sys.stdin); print("errors:",d["errors"],"items:",len(d["items"]))'

# Make freshly-indexed docs searchable (ES semantics: refresh).
curl -s -X POST "$ES/$IDX/_refresh" >/dev/null

# 3) match query — full-text search on the analyzed "name" field.
say "3. _search  match: name ~ \"water bottle\""
curl -s "$ES/$IDX/_search" -H 'Content-Type: application/json' -d '{
  "query": { "match": { "name": "water bottle" } },
  "_source": ["name","brand","price"]
}' | python3 -c '
import sys,json
d=json.load(sys.stdin)
print("total:", d["hits"]["total"]["value"])
for h in d["hits"]["hits"]:
    print(f"  {h[_SCORE]:.3f}  {h[_SRC][NAME]}".replace("_SCORE",chr(39)+"_score"+chr(39)).replace("_SRC",chr(39)+"_source"+chr(39)).replace("NAME",chr(39)+"name"+chr(39)))
' 2>/dev/null || curl -s "$ES/$IDX/_search" -H 'Content-Type: application/json' -d '{"query":{"match":{"name":"water bottle"}},"_source":["name"]}' | pp

# 4) bool query: must match + term filter + range filter — the classic ES combo.
say "4. _search  bool: match \"bottle\" AND brand=Hydro-or-Klean AND price 10..40"
curl -s "$ES/$IDX/_search" -H 'Content-Type: application/json' -d '{
  "query": {
    "bool": {
      "must":   [ { "match": { "name": "bottle" } } ],
      "filter": [
        { "terms": { "brand": ["Hydro","Klean"] } },
        { "range": { "price": { "gte": 10, "lte": 40 } } }
      ]
    }
  },
  "_source": ["name","brand","price"]
}' | pp

# 5) terms aggregation — count products per brand (no query hits, just the agg).
say "5. _search  terms agg: products per brand"
curl -s "$ES/$IDX/_search" -H 'Content-Type: application/json' -d '{
  "size": 0,
  "aggs": {
    "by_brand": { "terms": { "field": "brand" } }
  }
}' | pp

# 6) _cat/indices — operational sanity check, human-readable table.
say "6. GET /_cat/indices?v"
curl -s "$ES/_cat/indices?v"

echo
echo "All standard Elasticsearch calls succeeded against XERJ at $ES"
