# Quickstart — index, document, get, search

_Use case doc: quickstart_

Create an index with a mapping, write a document, read it back, and search it.

### ✅ create index w/ mapping

```bash
curl -s -XPUT "http://localhost:9200/articles" \
  -H 'content-type: application/json' \
  -d '{"mappings":{"properties":{"title":{"type":"text"},"body":{"type":"text"},"views":{"type":"integer"},"tag":{"type":"keyword"}}}}'
```

```json
{
  "acknowledged": true,
  "shards_acknowledged": true,
  "index": "articles"
}
```

_HTTP 200_

### ✅ index a document

```bash
curl -s -XPUT "http://localhost:9200/articles/_doc/1" \
  -H 'content-type: application/json' \
  -d '{"title":"Hello Xerj","body":"one binary for search","views":42,"tag":"intro"}'
```

```json
{
  "_index": "articles",
  "_id": "1",
  "_version": 1,
  "result": "created",
  "_shards": {
    "total": 1,
    "successful": 1,
    "failed": 0
  },
  "_seq_no": 1,
  "_primary_term": 1
}
```

_HTTP 201_

### ✅ get the document

```bash
curl -s "http://localhost:9200/articles/_doc/1"
```

```json
{
  "_index": "articles",
  "_id": "1",
  "_version": 1,
  "_seq_no": 1,
  "_primary_term": 1,
  "found": true,
  "_source": {
    "body": "one binary for search",
    "tag": "intro",
    "title": "Hello Xerj",
    "views": 42
  }
}
```

_HTTP 200_

### ✅ search it

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"query":{"match":{"body":"binary"}}}'
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
    "hits": [
      {
        "_id": "1",
        "_index": "articles",
        "_score": 0.28768211603164673,
        "_source": {
          "body": "one binary for search",
          "tag": "intro",
          "title": "Hello Xerj",
          "views": 42
        }
      }
    ],
    "max_score": 0.28768211603164673,
    "total": {
      "relation": "eq",
      "value": 1
    }
  },
  "timed_out": false,
  "took": 0
}
```

_HTTP 200_

