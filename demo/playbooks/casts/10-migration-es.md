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
green open .xerj_connections e405b691-43fd-4207-b8e4-1b1798f14f3b 1 0 0 0 256b 256b
green open .xerj_dashboards e4ee45eb-f00c-4061-b1c1-1887ee16cdc0 1 0 0 0 256b 256b
green open .xerj_api_tokens 0df2a84c-cf43-45cd-a857-3d69752afd50 1 0 0 0 256b 256b
green open logs-2026-04 b7645584-daa3-4b46-830b-0719a1e5cbd0 1 0 4 0 8497b 8497b
green open vstore 44c1282c-b2ef-418d-b1a3-20d4b8bdd7cd 1 0 3 0 4243b 4243b
green open bench a1dcded4-49f3-460a-bf0b-bcf0aa1dd2eb 1 0 4008 0 834527b 834527b
green open .xerj_sessions 631ca48a-3505-47af-a8d9-5361a63f4319 1 0 0 0 256b 256b
green open .xerj_idp_config 92214162-bf92-4c0c-af42-30a5e67b1afe 1 0 0 0 256b 256b
green open .xerj_passkeys 9bea314f-ac20-4b43-9c26-3584f6a4c289 1 0 0 0 256b 256b
green open .xerj_magic_links 0c4bbfcf-2ada-4a35-aa1f-0387483a770b 1 0 1 0 3428b 3428b
green open .xerj_views 8ad9dabe-4d9a-41c1-a2e4-2098c99f5e1f 1 0 0 0 256b 256b
green open articles 976bb39f-1296-4119-9795-ecd10067d302 1 0 5 0 7042b 7042b
green open .xerj_cluster_state 1ed40fcd-c764-4a3a-8c64-c00af958150a 1 0 0 0 256b 256b
green open .xerj_users 5aca9a28-2402-4b51-8dd7-7884d33d1be7 1 0 0 0 256b 256b
green open .xerj_alert_rules 1e4e1ebb-5f20-49a3-a5b6-6fa504f923d8 1 0 0 0 256b 256b
green open .xerj_prefs 3e4decad-d92a-428e-806e-a44f884abd6e 1 0 0 0 256b 256b
green open .xerj_alert_fires 724a4e2c-66a7-41dd-bc31-d85b30806561 1 0 0 0 256b 256b
green open .xerj_
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
  "_scroll_id": "54acf7d5-2b05-4759-a715-3a02025fead8",
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
        "_id": "d11e7ede-b068-4147-af2e-deaa4a498842",
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

