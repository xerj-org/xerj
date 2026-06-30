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
green open .xerj_magic_links f04e5ade-4fbc-443d-859e-445344634da3 1 0 1 0 3428b 3428b
green open .xerj_passkeys 686049de-3176-4935-b14b-bf7d3d82847d 1 0 0 0 256b 256b
green open .xerj_cluster_state 4910d8f4-fefc-4a7d-bfc1-03517410150d 1 0 0 0 256b 256b
green open .xerj_dashboards 196d95c7-e083-46a8-950d-44809ab0407d 1 0 0 0 256b 256b
green open .xerj_api_tokens 8909f869-ca02-483c-a922-657ad1e5bc0f 1 0 0 0 256b 256b
green open .xerj_prefs 0c054e84-a611-4ed0-8763-7d4e4aa670ba 1 0 0 0 256b 256b
green open .xerj_audit eae63fd9-3bed-4f5b-a8e2-0659f69b433c 1 0 0 0 256b 256b
green open .xerj_alert_fires 944735d9-7cab-4a96-a17b-48c7750ba1dc 1 0 0 0 256b 256b
green open vstore a9235792-d49d-4dc6-ba3d-c9bbd4b3bbe4 1 0 3 0 4243b 4243b
green open .xerj_connections a2b66a08-e3d0-4a40-b294-538c0370e4c4 1 0 0 0 256b 256b
green open .xerj_views 0550bd81-e152-4c6f-90a9-05bb4efd12f1 1 0 0 0 256b 256b
green open .xerj_sessions b11aedc0-c5a9-4537-8c1f-43fe153a526f 1 0 0 0 256b 256b
green open .xerj_idp_config f758cab4-93bc-402b-941d-ff5c1f138d08 1 0 0 0 256b 256b
green open .xerj_alert_rules 49502ac9-e17a-4ad4-9844-e2728fe4037b 1 0 0 0 256b 256b
green open articles 145b2f9a-305b-4d26-9f56-61ba276abc68 1 0 5 0 8285b 8285b
green open .xerj_users 5ccd189b-10eb-493c-ac58-eddaacb3a12f 1 0 0 0 256b 256b
green open logs-2026-04 4c531df0-221a-4cd3-ade6-1621114e0ab6 1 0 4 0 8497b 8497b
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
  "_scroll_id": "7b070e1b-b2ff-4c55-a9bf-02c32728bf70",
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
        "_id": "c22c6d77-a257-4abd-825d-8d9560e34340",
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

