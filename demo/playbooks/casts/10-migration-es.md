# Migration from Elasticsearch — drop-in parity

_Use case doc: migration-from-es_

The exact ES request bodies return ES-shaped responses — clients, Logstash, Kibana keep working.

### ✅ ES version handshake (Kibana checks this)

```bash
curl -s "http://localhost:9200/"
```

```json
{
  "name": "xerj-node-c69932ac",
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
green open .xerj_users 15eeb6ba-d851-4bdc-bf09-8880a062053e 1 0 0 0
green open .xerj_prefs d43de18b-368e-4cc5-ba42-494607ba17ba 1 0 0 0
green open .xerj_connections 0afdeeaf-fa34-41eb-a509-76f53c8e5627 1 0 0 0
green open .xerj_magic_links 381a4093-cf49-4d92-b87e-e75b73a48f18 1 0 1 0
green open .xerj_sessions 8d7776ef-6b9e-45fa-b8ef-3efea8f71622 1 0 0 0
green open .xerj_views 0e6edae4-8f94-4dbd-9fd7-041eed5023b2 1 0 0 0
green open articles d4d4d2de-6262-4996-8b1f-21f8e3044818 1 0 5 0
green open .xerj_api_tokens e55b7c3b-1f86-4943-8fcd-a20aeac4f756 1 0 0 0
green open vstore 7a0e45d3-7433-4d3e-a154-cdbf955fd82b 1 0 3 0
green open logs-2026-04 d74aeacb-c3da-4193-b085-dc6ccf9f3b9d 1 0 4 0
green open .xerj_idp_config a0f89379-2241-4060-9edf-571782d6d64c 1 0 0 0
green open .xerj_alert_fires d10d3f93-84b6-4601-a9c4-52a3ddd7f2f4 1 0 0 0
green open .xerj_passkeys e23768a0-62cd-4ba2-94c9-05a5b05e5129 1 0 0 0
green open .xerj_dashboards de56da56-c4e6-4c70-a489-03ccd18b1d79 1 0 0 0
green open .xerj_alert_rules 09b41441-f5fe-4c5c-a4e7-62da16e6ac4d 1 0 0 0
green open .xerj_audit 426e8c6e-165e-4e1c-a70a-e7c9eafc6b1c 1 0 0 0
green open .xerj_cluster_state 05bc1d6c-b330-4b80-9706-3f390ca66952 1 0 0 0
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
  "_scroll_id": "3957ab5f-09e3-4bef-8e45-2986249a0089",
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
        "_id": "6e505def-8b8b-4048-a7bb-449a149f8af5",
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

