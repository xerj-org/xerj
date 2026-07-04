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
green open .xerj_views 88ea9cae-3df7-4f1d-b072-74f3bdd26b5c 1 0 0 0 256b 256b
green open vstore 6c200616-e5f0-4563-b875-1a71986545c3 1 0 3 0 4243b 4243b
green open .xerj_passkeys bf5550cb-15ed-4093-8f5d-b2acf766cc77 1 0 0 0 256b 256b
green open .xerj_sessions 6fe9df8f-ec2c-4751-ae2a-a174d785381c 1 0 0 0 256b 256b
green open .xerj_dashboards c3e392d9-5cce-4ff5-9378-d02fb278b944 1 0 0 0 256b 256b
green open articles f85ec8d5-6479-414c-ab9d-3b96be5c93d6 1 0 5 0 8284b 8284b
green open .xerj_idp_config e2f73d10-a34a-4d3a-99f9-1e889e6879cc 1 0 0 0 256b 256b
green open .xerj_alert_rules fda69083-2c9d-4d86-9b69-e7f0a16c6106 1 0 0 0 256b 256b
green open .xerj_connections 586631c4-bd3a-4b7e-92ad-cf1fa60b154b 1 0 0 0 256b 256b
green open .xerj_cluster_state 5c6795d4-4d5a-464b-ac6c-1e1d33c6ffe6 1 0 0 0 256b 256b
green open .xerj_alert_fires 8d7b04ae-e58b-457d-9cac-75727e453634 1 0 0 0 256b 256b
green open logs-2026-04 1db6bbeb-3fc7-491d-8ce0-9946da6b0df2 1 0 4 0 7117b 7117b
green open .xerj_prefs a0b01d2c-bfe4-4ffb-b5ad-aab1b14a8281 1 0 0 0 256b 256b
green open .xerj_magic_links f58ea8a1-05f4-4f0a-84a3-1334883d21e2 1 0 1 0 3429b 3429b
green open .xerj_users c6625bf8-6ad4-4b4b-b2f2-48bbeb0c086a 1 0 0 0 256b 256b
green open bench 057c0c46-2124-4f3a-a6c7-a0345fc702ac 1 0 4008 0 849235b 849235b
green open .xerj_audit 13237196-6c09-45fb-b301-e182a6152954 1 0 0 0 256b 256b
green open .xerj_api_t
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
  "_scroll_id": "7cf46fb4-d1dc-4b79-9e1a-2b044113c384",
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
        "_id": "a42a64ea-0f2c-44b2-b4af-3db93b145cf3",
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

