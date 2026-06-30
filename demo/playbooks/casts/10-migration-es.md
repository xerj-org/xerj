# Migration from Elasticsearch — drop-in parity

_Use case doc: migration-from-es_

The exact ES request bodies return ES-shaped responses — clients, Logstash, Kibana keep working.

### ✅ ES version handshake (Kibana checks this)

```bash
curl -s "http://localhost:9200/"
```

```json
{
  "name": "local",
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
green open logs-2026-04 62f88b3a-9919-4a48-8610-7bf3cc3501bb 1 0 4 0 7144b 7144b
green open .xerj_views 0300e546-2f3e-4789-b250-1c7667d8df9a 1 0 0 0 256b 256b
green open .xerj_passkeys f2e59007-4544-42d6-b276-a7780751e70b 1 0 0 0 256b 256b
green open .xerj_api_tokens bd49f42f-cdf0-41c2-81bb-f9b2f9e077d8 1 0 0 0 256b 256b
green open bench 59f56421-0ee9-469c-890c-b9cef49a4ea3 1 0 4008 0 834617b 834617b
green open articles dde79c2a-70ce-4ed4-b7a7-ef7341d59e3c 1 0 5 0 7933b 7933b
green open .xerj_alert_rules b4509978-8e31-465f-b28c-826977d7db2e 1 0 0 0 256b 256b
green open .xerj_magic_links 1a732437-0d98-4ddf-8785-8a435d2ab8f6 1 0 1 0 3429b 3429b
green open vstore 2121c20b-47f8-4c78-ac02-96b14e20222c 1 0 3 0 4267b 4267b
green open .xerj_audit 16cec347-3de4-4998-9498-f1fd01c5f093 1 0 0 0 256b 256b
green open .xerj_users 500a738f-b5cb-471d-8c08-7cc69505c5e5 1 0 0 0 256b 256b
green open .xerj_idp_config ad2ae30a-d148-40ab-bf62-7b4ca908fd8c 1 0 0 0 256b 256b
green open .xerj_cluster_state c795ae7d-d9d7-4833-91bf-883c1275c5a0 1 0 0 0 256b 256b
green open .xerj_dashboards b0e02c8b-98c2-4c0e-a7bc-52998ef8c107 1 0 0 0 256b 256b
green open .xerj_prefs 720b714c-4007-45c7-a0f5-0241cedb9257 1 0 0 0 256b 256b
green open .xerj_sessions 3caac48f-1c1a-40c1-ad20-f50959432802 1 0 0 0 256b 256b
green open .xerj_alert_fires a4ed78e9-8e3a-4c6f-a900-5732bb944d2a 1 0 0 0 256b 256b
green open .xerj_connec
… (60 more bytes)
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
  "_scroll_id": "a9ea2f23-6479-4548-83cd-f9b6f8861e3b",
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

