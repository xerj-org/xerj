# Aggregations — the boardroom view

_Use case doc: aggregations_

Metric + bucket aggregations over the live index.

### ✅ terms

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"size":0,"aggs":{"by_tag":{"terms":{"field":"tag"}}}}'
```

```json
{
  "_shards": {
    "failed": 0,
    "skipped": 0,
    "successful": 1,
    "total": 1
  },
  "aggregations": {
    "by_tag": {
      "buckets": [
        {
          "doc_count": 2,
          "key": "a"
        },
        {
          "doc_count": 2,
          "key": "b"
        },
        {
          "doc_count": 1,
          "key": "intro"
        }
      ],
      "doc_count_error_upper_bound": 0,
      "sum_other_doc_count": 0
    }
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

### ✅ stats

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"size":0,"aggs":{"v":{"stats":{"field":"views"}}}}'
```

```json
{
  "_shards": {
    "failed": 0,
    "skipped": 0,
    "successful": 1,
    "total": 1
  },
  "aggregations": {
    "v": {
      "avg": 28.4,
      "count": 5,
      "max": 42,
      "min": 10,
      "sum": 142
    }
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

### ✅ sum / avg / max / min

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"size":0,"aggs":{"s":{"sum":{"field":"views"}},"a":{"avg":{"field":"views"}},"mx":{"max":{"field":"views"}},"mn":{"min":{"field":"views"}}}}'
```

```json
{
  "_shards": {
    "failed": 0,
    "skipped": 0,
    "successful": 1,
    "total": 1
  },
  "aggregations": {
    "a": {
      "value": 28.4
    },
    "mn": {
      "value": 10
    },
    "mx": {
      "value": 42
    },
    "s": {
      "value": 142
    }
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

### ✅ cardinality

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"size":0,"aggs":{"uniq":{"cardinality":{"field":"tag"}}}}'
```

```json
{
  "_shards": {
    "failed": 0,
    "skipped": 0,
    "successful": 1,
    "total": 1
  },
  "aggregations": {
    "uniq": {
      "value": 3
    }
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

### ✅ value_count

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"size":0,"aggs":{"c":{"value_count":{"field":"views"}}}}'
```

```json
{
  "_shards": {
    "failed": 0,
    "skipped": 0,
    "successful": 1,
    "total": 1
  },
  "aggregations": {
    "c": {
      "value": 5
    }
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

### ✅ histogram

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"size":0,"aggs":{"h":{"histogram":{"field":"views","interval":10}}}}'
```

```json
{
  "_shards": {
    "failed": 0,
    "skipped": 0,
    "successful": 1,
    "total": 1
  },
  "aggregations": {
    "h": {
      "buckets": [
        {
          "doc_count": 1,
          "key": 10
        },
        {
          "doc_count": 1,
          "key": 20
        },
        {
          "doc_count": 1,
          "key": 30
        },
        {
          "doc_count": 2,
          "key": 40
        }
      ]
    }
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

### ✅ range

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"size":0,"aggs":{"r":{"range":{"field":"views","ranges":[{"to":20},{"from":20}]}}}}'
```

```json
{
  "_shards": {
    "failed": 0,
    "skipped": 0,
    "successful": 1,
    "total": 1
  },
  "aggregations": {
    "r": {
      "buckets": [
        {
          "doc_count": 1,
          "key": "*-20.0",
          "to": 20
        },
        {
          "doc_count": 4,
          "from": 20,
          "key": "20.0-*"
        }
      ]
    }
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

### ✅ percentiles

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"size":0,"aggs":{"p":{"percentiles":{"field":"views"}}}}'
```

```json
{
  "_shards": {
    "failed": 0,
    "skipped": 0,
    "successful": 1,
    "total": 1
  },
  "aggregations": {
    "p": {
      "values": {
        "1.0": 10.4,
        "25.0": 20,
        "5.0": 12,
        "50.0": 30,
        "75.0": 40,
        "95.0": 41.6,
        "99.0": 41.92
      }
    }
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

### ✅ terms + nested avg

```bash
curl -s -XPOST "http://localhost:9200/articles/_search" \
  -H 'content-type: application/json' \
  -d '{"size":0,"aggs":{"by_tag":{"terms":{"field":"tag"},"aggs":{"avg_views":{"avg":{"field":"views"}}}}}}'
```

```json
{
  "_shards": {
    "failed": 0,
    "skipped": 0,
    "successful": 1,
    "total": 1
  },
  "aggregations": {
    "by_tag": {
      "buckets": [
        {
          "avg_views": {
            "value": 20
          },
          "doc_count": 2,
          "key": "a"
        },
        {
          "avg_views": {
            "value": 30
          },
          "doc_count": 2,
          "key": "b"
        },
        {
          "avg_views": {
            "value": 42
          },
          "doc_count": 1,
          "key": "intro"
        }
      ],
      "doc_count_error_upper_bound": 0,
      "sum_other_doc_count": 0
    }
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

