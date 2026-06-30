# Log analytics — Splunk/Kibana workload

_Use case doc: playbooks/log-analytics_

Template-driven log index, time-series + level breakdown aggregations.

### ✅ bulk ingest logs (template applies)

```bash
curl -s -XPOST "http://localhost:9200/logs-2026-04/_bulk" \
  -H 'content-type: application/x-ndjson' \
  --data-binary $'{"index":{}}\n{"@timestamp":"2026-04-26T19:47:24Z","level":"info","msg":"request ok","svc":"api"}\n{"index":{}}\n{"@timestamp":"2026-04-26T19:47:24Z","level":"error","msg":"timeout","svc":"api"}\n{"index":{}}\n{"@timestamp":"2026-04-26T19:47:24Z","level":"error","msg":"5xx upstream","svc":"web"}\n{"index":{}}\n{"@timestamp":"2026-04-26T19:47:24Z","level":"warn","msg":"slow query","svc":"db"}\n'
```

```json
{
  "errors": false,
  "items": [
    {
      "index": {
        "_id": "23ce61e1-22c8-4638-bd85-1549f7bbdc16",
        "_index": "logs-2026-04",
        "_primary_term": 1,
        "_seq_no": 1782803695187901,
        "_shards": {
          "failed": 0,
          "successful": 1,
          "total": 1
        },
        "_version": 1,
        "result": "created",
        "status": 201
      }
    },
    {
      "index": {
        "_id": "f955a504-56c5-4662-8822-73b062fe56c1",
        "_index": "logs-2026-04",
        "_primary_term": 1,
        "_seq_no": 1782803695187902,
        "_shards": {
          "failed": 0,
          "successful": 1,
          "total": 1
        },
        "_version": 1,
        "result": "created",
        "status": 201
      }
    },
    {
      "index": {
        "_id": "9392ccfd-8c48-4d6d-9927-f49c93613185",
        "_index": "logs-2026-04",
        "_primary_term": 1,
        "_seq_no": 1782803695187902,
        "_shards": {
          "failed": 0,
          "successful": 1,
          "total": 1
        },
        "_version": 1,
        "result": "created",
        "status": 201
      }
    },
    {
      "index": {
        "_id": "523f044d-430f-40d3-8b77-a9e022ff128f",
        "_index": "logs-2026-04",
        "_primary_term": 1,
        "_seq_no": 1782803695187902,
        "_shards": {
          "failed": 0,
          "successful": 1,
          "
… (127 more bytes)
```

_HTTP 200_

### ✅ refresh

```bash
curl -s -XPOST "http://localhost:9200/logs-2026-04/_refresh"
```

```json
{
  "_shards": {
    "failed": 0,
    "successful": 1,
    "total": 1
  }
}
```

_HTTP 200_

### ✅ errors only (bool filter)

```bash
curl -s -XPOST "http://localhost:9200/logs-2026-04/_search" \
  -H 'content-type: application/json' \
  -d '{"query":{"term":{"level":"error"}}}'
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
        "_id": "f955a504-56c5-4662-8822-73b062fe56c1",
        "_index": "logs-2026-04",
        "_score": 1.6931471824645996,
        "_source": {
          "@timestamp": "2026-04-26T19:47:24Z",
          "level": "error",
          "msg": "timeout",
          "svc": "api"
        }
      },
      {
        "_id": "9392ccfd-8c48-4d6d-9927-f49c93613185",
        "_index": "logs-2026-04",
        "_score": 1.6931471824645996,
        "_source": {
          "@timestamp": "2026-04-26T19:47:24Z",
          "level": "error",
          "msg": "5xx upstream",
          "svc": "web"
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

### ✅ count by level

```bash
curl -s -XPOST "http://localhost:9200/logs-2026-04/_search" \
  -H 'content-type: application/json' \
  -d '{"size":0,"aggs":{"by_level":{"terms":{"field":"level"}}}}'
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
    "by_level": {
      "buckets": [
        {
          "doc_count": 2,
          "key": "error"
        },
        {
          "doc_count": 1,
          "key": "info"
        },
        {
          "doc_count": 1,
          "key": "warn"
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
      "value": 4
    }
  },
  "timed_out": false,
  "took": 0
}
```

_HTTP 200_

### ✅ errors over time (date_histogram)

```bash
curl -s -XPOST "http://localhost:9200/logs-2026-04/_search" \
  -H 'content-type: application/json' \
  -d '{"size":0,"query":{"term":{"level":"error"}},"aggs":{"t":{"date_histogram":{"field":"@timestamp","calendar_interval":"day"}}}}'
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
    "t": {
      "buckets": [
        {
          "doc_count": 2,
          "key": 1777161600000,
          "key_as_string": "2026-04-26T00:00:00.000Z"
        }
      ]
    }
  },
  "hits": {
    "hits": [],
    "max_score": null,
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

