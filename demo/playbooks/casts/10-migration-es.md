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
green open .xerj_alert_fires 60a28f65-8ad3-47ea-a143-b0e2cd77211c 1 0 0 0 256b 256b
green open .xerj_cluster_state 46e2b940-8d5a-4eec-8e1e-7536b4c72513 1 0 0 0 256b 256b
green open .xerj_api_tokens c98376de-7dee-4591-8798-d55031596277 1 0 0 0 256b 256b
green open .xerj_sessions 25708e4e-8eb4-48f2-bb79-75afe127cf1a 1 0 0 0 256b 256b
green open .xerj_audit fb3e3dbe-ef99-49ca-b262-9fac0fc195dd 1 0 0 0 256b 256b
green open .xerj_passkeys 2a4d157b-a05f-4b6b-a8a0-4212dc6f2c33 1 0 0 0 256b 256b
green open .xerj_magic_links fe52ebed-4be1-4ef3-806a-c68ef7e5b375 1 0 1 0 3428b 3428b
green open .xerj_dashboards c0802f3b-d8fc-4a54-95c6-3c6b08860bf3 1 0 0 0 256b 256b
green open logs-2026-04 b62f7b1e-0d03-424c-afb2-60db93e476cf 1 0 4 0 8145b 8145b
green open .xerj_views 5fc8ef22-c7ca-4de1-aa2d-ee1a34d44165 1 0 0 0 256b 256b
green open articles f6e96209-5ae3-4512-8eba-376bce59e39e 1 0 5 0 8284b 8284b
green open .xerj_idp_config 9b573702-0fa0-4c04-ad4e-294688206190 1 0 0 0 256b 256b
green open .xerj_prefs 013340fe-9f77-423e-a4af-ccc3d2d77d7a 1 0 0 0 256b 256b
green open vstore 0c2edad2-ca44-4029-a15b-734bc20ac812 1 0 3 0 4243b 4243b
green open .xerj_users a737929b-e3ed-4746-be80-0958c6816aab 1 0 0 0 256b 256b
green open .xerj_alert_rules 6fb717ea-478d-424d-8f6d-3dd6658bf46f 1 0 0 0 256b 256b
green open bench fb067324-110b-4254-ae89-4539f7047911 1 0 4008 0 834790b 834790b
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
  "_scroll_id": "38cee66d-23ed-437d-8c13-13c0b7b9764d",
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
        "_id": "cb1a87fa-b677-42f9-a436-c015b2d100e5",
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

