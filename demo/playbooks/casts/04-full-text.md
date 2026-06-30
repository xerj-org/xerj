# Full-text search — the ES query families

_Use case doc: queries / playbooks/full-text_

The query DSL your Discover muscle memory already knows.

### ✅ match

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"query":{"match":{"body":"search"}}}'
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
      },
      {
        "_id": "fe733455-65ba-4344-ba9d-098a7ba9de28",
        "_index": "articles",
        "_score": 0.28768211603164673,
        "_source": {
          "body": "beta search",
          "tag": "b",
          "title": "B",
          "views": 20
        }
      }
    ],
    "max_score": 0.28768211603164673,
    "total": {
      "relation": "eq",
      "value": 2
    }
  },
  "timed_out": false,
  "took": 1
}
```

_HTTP 200_

### ✅ match_phrase

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"query":{"match_phrase":{"body":"alpha vector"}}}'
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
        "_id": "01ddaa6c-b55e-4744-a7b2-538577fe3fe8",
        "_index": "articles",
        "_score": 1,
        "_source": {
          "body": "alpha vector",
          "tag": "a",
          "title": "A",
          "views": 10
        }
      }
    ],
    "max_score": 1,
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

### ✅ term (keyword)

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"query":{"term":{"tag":"a"}}}'
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
        "_id": "01ddaa6c-b55e-4744-a7b2-538577fe3fe8",
        "_index": "articles",
        "_score": 1.6931471824645996,
        "_source": {
          "body": "alpha vector",
          "tag": "a",
          "title": "A",
          "views": 10
        }
      },
      {
        "_id": "5a5e1107-526e-469c-8bdb-db19781d5b4a",
        "_index": "articles",
        "_score": 1.6931471824645996,
        "_source": {
          "body": "gamma logs",
          "tag": "a",
          "title": "C",
          "views": 30
        }
      }
    ],
    "max_score": 1.6931471824645996,
    "total": {
      "relation": "eq",
      "value": 2
    }
  },
  "timed_out": false,
  "took": 0
}
```

_HTTP 200_

### ✅ terms

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"query":{"terms":{"tag":["a","b"]}}}'
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
        "_id": "01ddaa6c-b55e-4744-a7b2-538577fe3fe8",
        "_index": "articles",
        "_score": 1,
        "_source": {
          "body": "alpha vector",
          "tag": "a",
          "title": "A",
          "views": 10
        }
      },
      {
        "_id": "fe733455-65ba-4344-ba9d-098a7ba9de28",
        "_index": "articles",
        "_score": 1,
        "_source": {
          "body": "beta search",
          "tag": "b",
          "title": "B",
          "views": 20
        }
      },
      {
        "_id": "5a5e1107-526e-469c-8bdb-db19781d5b4a",
        "_index": "articles",
        "_score": 1,
        "_source": {
          "body": "gamma logs",
          "tag": "a",
          "title": "C",
          "views": 30
        }
      },
      {
        "_id": "d9fee133-5834-4a29-b4d1-a337ffaabd41",
        "_index": "articles",
        "_score": 1,
        "_source": {
          "body": "delta agg",
          "tag": "b",
          "title": "D",
          "views": 40
        }
      }
    ],
    "max_score": 1,
    "total": {
      "relation": "eq",
      "value": 4
    }
  },
  "timed_out": false,
  "took": 0
}
```

_HTTP 200_

### ✅ range

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"query":{"range":{"views":{"gte":20}}}}'
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
        "_score": 1,
        "_source": {
          "body": "one binary for search",
          "tag": "intro",
          "title": "Hello Xerj",
          "views": 42
        }
      },
      {
        "_id": "fe733455-65ba-4344-ba9d-098a7ba9de28",
        "_index": "articles",
        "_score": 1,
        "_source": {
          "body": "beta search",
          "tag": "b",
          "title": "B",
          "views": 20
        }
      },
      {
        "_id": "5a5e1107-526e-469c-8bdb-db19781d5b4a",
        "_index": "articles",
        "_score": 1,
        "_source": {
          "body": "gamma logs",
          "tag": "a",
          "title": "C",
          "views": 30
        }
      },
      {
        "_id": "d9fee133-5834-4a29-b4d1-a337ffaabd41",
        "_index": "articles",
        "_score": 1,
        "_source": {
          "body": "delta agg",
          "tag": "b",
          "title": "D",
          "views": 40
        }
      }
    ],
    "max_score": 1,
    "total": {
      "relation": "eq",
      "value": 4
    }
  },
  "timed_out": false,
  "took": 0
}
```

_HTTP 200_

### ✅ bool must+filter

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"query":{"bool":{"must":[{"match":{"body":"logs"}}],"filter":[{"term":{"tag":"a"}}]}}}'
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
        "_id": "5a5e1107-526e-469c-8bdb-db19781d5b4a",
        "_index": "articles",
        "_score": 0.9741763472557068,
        "_source": {
          "body": "gamma logs",
          "tag": "a",
          "title": "C",
          "views": 30
        }
      }
    ],
    "max_score": 0.9741763472557068,
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

### ✅ prefix

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"query":{"prefix":{"body":"alp"}}}'
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
        "_id": "01ddaa6c-b55e-4744-a7b2-538577fe3fe8",
        "_index": "articles",
        "_score": 1,
        "_source": {
          "body": "alpha vector",
          "tag": "a",
          "title": "A",
          "views": 10
        }
      }
    ],
    "max_score": 1,
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

### ✅ wildcard

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"query":{"wildcard":{"body":"sea*"}}}'
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
        "_score": 1,
        "_source": {
          "body": "one binary for search",
          "tag": "intro",
          "title": "Hello Xerj",
          "views": 42
        }
      },
      {
        "_id": "fe733455-65ba-4344-ba9d-098a7ba9de28",
        "_index": "articles",
        "_score": 1,
        "_source": {
          "body": "beta search",
          "tag": "b",
          "title": "B",
          "views": 20
        }
      }
    ],
    "max_score": 1,
    "total": {
      "relation": "eq",
      "value": 2
    }
  },
  "timed_out": false,
  "took": 0
}
```

_HTTP 200_

### ✅ exists

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"query":{"exists":{"field":"title"}}}'
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
        "_score": 1,
        "_source": {
          "body": "one binary for search",
          "tag": "intro",
          "title": "Hello Xerj",
          "views": 42
        }
      },
      {
        "_id": "01ddaa6c-b55e-4744-a7b2-538577fe3fe8",
        "_index": "articles",
        "_score": 1,
        "_source": {
          "body": "alpha vector",
          "tag": "a",
          "title": "A",
          "views": 10
        }
      },
      {
        "_id": "fe733455-65ba-4344-ba9d-098a7ba9de28",
        "_index": "articles",
        "_score": 1,
        "_source": {
          "body": "beta search",
          "tag": "b",
          "title": "B",
          "views": 20
        }
      },
      {
        "_id": "5a5e1107-526e-469c-8bdb-db19781d5b4a",
        "_index": "articles",
        "_score": 1,
        "_source": {
          "body": "gamma logs",
          "tag": "a",
          "title": "C",
          "views": 30
        }
      },
      {
        "_id": "d9fee133-5834-4a29-b4d1-a337ffaabd41",
        "_index": "articles",
        "_score": 1,
        "_source": {
          "body": "delta agg",
          "tag": "b",
          "title": "D",
          "views": 40
        }
      }
    ],
    "max_score
… (108 more bytes)
```

_HTTP 200_

### ✅ highlight + sort + _source

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"query":{"match":{"body":"vector"}},"highlight":{"fields":{"body":{}}},"sort":[{"views":"desc"}],"_source":["title","views"]}'
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
        "_id": "01ddaa6c-b55e-4744-a7b2-538577fe3fe8",
        "_index": "articles",
        "_score": 0.28768211603164673,
        "_source": {
          "title": "A",
          "views": 10
        },
        "sort": [
          10
        ]
      }
    ],
    "max_score": null,
    "total": {
      "relation": "eq",
      "value": 1
    }
  },
  "timed_out": false,
  "took": 1
}
```

_HTTP 200_

