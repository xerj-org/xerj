# Mappings, templates & aliases

_Use case doc: config / operations_

Schema introspection + index templates + aliases — ES-identical.

### ✅ get mapping

```bash
curl -s "http://localhost:9200/articles/_mapping"
```

```json
{
  "articles": {
    "mappings": {
      "properties": {
        "body": {
          "type": "text"
        },
        "tag": {
          "type": "keyword"
        },
        "title": {
          "type": "text"
        },
        "views": {
          "type": "integer"
        }
      }
    }
  }
}
```

_HTTP 200_

### ✅ put index template

```bash
curl -s -XPUT "http://localhost:9200/_index_template/logs_tpl" \
  -H 'content-type: application/json' \
  -d '{"index_patterns":["logs-*"],"template":{"mappings":{"properties":{"@timestamp":{"type":"date"},"level":{"type":"keyword"},"msg":{"type":"text"}}}}}'
```

```json
{
  "acknowledged": true
}
```

_HTTP 200_

### ✅ add alias

```bash
curl -s -XPOST "http://localhost:9200/_aliases" \
  -H 'content-type: application/json' \
  -d '{"actions":[{"add":{"index":"articles","alias":"articles_live"}}]}'
```

```json
{
  "acknowledged": true
}
```

_HTTP 200_

### ✅ search via alias

```bash
curl -s -XPOST "http://localhost:9200/articles_live/_search" \
  -H 'content-type: application/json' \
  -d '{"size":0,"query":{"match_all":{}}}'
```

```json
{
  "_shards": {
    "failed": 0,
    "skipped": 0,
    "successful": 1,
    "total": 1
  },
  "hits": {
    "hits": [],
    "max_score": null,
    "total": {
      "relation": "eq",
      "value": 5
    }
  },
  "timed_out": false,
  "took": 0
}
```

_HTTP 200_

