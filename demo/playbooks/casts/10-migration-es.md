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
green open .xerj_cluster_state 612ff274-fddf-469f-9714-bd3e6e4d2274 1 0 0 0 256b 256b
green open .xerj_sessions 4263400a-0c99-4f3f-9eda-f90d031ee3b5 1 0 0 0 256b 256b
green open .xerj_alert_rules 63e3096a-4393-4b65-836a-8e9e15a6feec 1 0 0 0 256b 256b
green open .xerj_audit 715011a1-7180-4113-a7bd-8eb8cb21ad02 1 0 0 0 256b 256b
green open bench 7c1bcd12-1cab-4dd9-bd41-eaf3284039a6 1 0 4008 0 834761b 834761b
green open .xerj_prefs 9f9f24ac-418a-4da9-92d1-76b665cc14c3 1 0 0 0 256b 256b
green open .xerj_dashboards 4b2a4f2a-f9ab-49b4-9a80-43df1b289c32 1 0 0 0 256b 256b
green open .xerj_api_tokens 2b3051fa-6f0a-4a8c-a88c-4e235fbeca8b 1 0 0 0 256b 256b
green open .xerj_views 08790e5f-3b11-493e-ab7c-9c3e6698ae6d 1 0 0 0 256b 256b
green open .xerj_users 778bfe3d-d818-441e-9532-38d275c67f19 1 0 0 0 256b 256b
green open logs-2026-04 a0f8a075-5487-45a5-939c-8d3444a2bc18 1 0 4 0 8497b 8497b
green open .xerj_passkeys d6d3447a-77d3-4e69-a708-f56239fc1091 1 0 0 0 256b 256b
green open vstore 09425fc9-fcc2-484c-82bd-e0eb94934db0 1 0 3 0 4247b 4247b
green open .xerj_alert_fires 90d4e93d-1212-4ad7-8b2e-f45bcaa748dd 1 0 0 0 256b 256b
green open .xerj_idp_config 2d5c5ef7-d45f-4fdd-aa3c-298a3b43505a 1 0 0 0 256b 256b
green open .xerj_magic_links c373c0c8-1339-470b-9398-990fa548bb44 1 0 1 0 3428b 3428b
green open articles fca027c7-a98c-4459-9422-959fa07752db 1 0 5 0 5713b 5713b
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
  "_scroll_id": "1938ac8d-4758-411f-8987-a669f0145905",
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
        "_id": "55e198e7-da9e-42ec-bf14-b12c60b4f2a6",
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

