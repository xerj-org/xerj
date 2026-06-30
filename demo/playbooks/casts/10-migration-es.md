# Migration from Elasticsearch — drop-in parity

_Use case doc: migration-from-es_

The exact ES request bodies return ES-shaped responses — clients, Logstash, Kibana keep working.

### ✅ ES version handshake (Kibana checks this)

```bash
curl -s "http://localhost:9200/"
```

```json
{
  "name": "xerj-node-66d6c077",
  "cluster_name": "xerj",
  "cluster_uuid": "xerj-cluster-0000-0000-0000-000000000000",
  "version": {
    "number": "8.13.0",
    "build_flavor": "default",
    "build_type": "tar",
    "build_hash": "00000000",
    "build_date": "2024-03-22T00:00:00.000Z",
    "build_snapshot": false,
    "lucene_version": "9.10.0",
    "minimum_wire_compatibility_version": "7.17.0",
    "minimum_index_compatibility_version": "7.0.0"
  },
  "tagline": "You Know, for Search"
}
```

_HTTP 200_

### ✅ _cat/indices (ops tooling)

```bash
curl -s "http://localhost:9200/_cat/indices?v"
```

```json
green open .xerj_cluster_state 96c36337-d6b1-47f9-9905-e356b6cecb04 1 0 0 0
green open .xerj_api_tokens 1899253e-420a-40a1-af69-6bee59291936 1 0 0 0
green open .xerj_prefs 12f27538-0e09-44e0-aa4b-a0bf44914b23 1 0 0 0
green open logs-2026-04 f00d351d-25ca-465f-99d6-945449e0985e 1 0 4 0
green open articles 1d2028e5-ec5e-4841-bb85-3dc07ba0ee45 1 0 5 0
green open .xerj_dashboards c1be10bb-e64c-4474-bb9b-1314b7a507b4 1 0 0 0
green open .xerj_users c8abbbd2-e317-4e87-9ee4-1cb3f76a8bd5 1 0 0 0
green open .xerj_passkeys ab84e4b4-c7f5-477a-8adc-bcdd2a46822c 1 0 0 0
green open vstore a0fc834d-e792-43ad-ab29-ab698d0d7124 1 0 3 0
green open .xerj_alert_rules 175ed6ba-f804-4b85-b402-b28d4c02ff8d 1 0 0 0
green open .xerj_magic_links 5d8ee201-a207-4952-ae6f-f9d013978b41 1 0 1 0
green open .xerj_idp_config 9c1e2518-68ef-495e-8e9f-70588dac66cc 1 0 0 0
green open .xerj_sessions 239d4dc1-fa8a-49de-8e5c-36d451fc3907 1 0 0 0
green open .xerj_audit 3fd87f94-f6a7-4dda-b7c0-5ee14d7b0c3a 1 0 0 0
green open .xerj_connections 776f68be-7795-4db5-8c7c-029ce4d03fcd 1 0 0 0
green open .xerj_alert_fires e5705c2b-ee99-4d42-b04a-0733bbc0b421 1 0 0 0
green open .xerj_views 3293e930-eb61-4342-88b7-d2ac2347ee69 1 0 0 0
```

_HTTP 200_

### ✅ scroll API (pagination)

```bash
curl -s -XPOST "http://localhost:9200/articles/_search?scroll=1m" \
  -H 'content-type: application/json' \
  -d '{"size":2,"query":{"match_all":{}}}'
```

```json
{
  "_scroll_id": "56506e1c-84f3-4665-88f1-f7fcf5ddedfa",
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
        "_id": "aab064f6-eecd-4b53-a75c-bf507f99edae",
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
      "value": 5
    }
  },
  "timed_out": false,
  "took": 0
}
```

_HTTP 200_

### ✅ _delete_by_query

```bash
curl -s -XPOST "http://localhost:9200/articles/_delete_by_query" \
  -H 'content-type: application/json' \
  -d '{"query":{"term":{"tag":"b"}}}'
```

```json
{
  "batches": 1,
  "deleted": 2,
  "failures": [],
  "noops": 0,
  "requests_per_second": -1,
  "throttled_millis": 0,
  "throttled_until_millis": 0,
  "timed_out": false,
  "took": 0,
  "total": 2,
  "version_conflicts": 0
}
```

_HTTP 200_

