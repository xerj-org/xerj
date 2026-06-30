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
green open .xerj_api_tokens cbd6449b-6e18-41e7-aeef-4bbf9bd0a1fb 1 0 0 0 256b 256b
green open .xerj_magic_links 2d9d6650-8c2e-451f-804f-13e5e419e429 1 0 1 0 3429b 3429b
green open .xerj_connections b4d108f9-ce76-43ca-b5dd-4aefdddfbf93 1 0 0 0 256b 256b
green open vstore aac8fd11-2800-4f52-9a41-f619d3795680 1 0 3 0 4243b 4243b
green open .xerj_sessions 868fa9f9-890a-4174-8f9d-7a65ed4fa61f 1 0 0 0 256b 256b
green open logs-2026-04 8cd6555e-82f4-473c-b0f7-ece61035cb18 1 0 4 0 8497b 8497b
green open .xerj_prefs 9842c246-277d-4787-91cc-04c68907410d 1 0 0 0 256b 256b
green open articles 69fbdee8-17dc-4de6-b210-6430a926439e 1 0 5 0 7050b 7050b
green open .xerj_users 1ff1fd4e-0273-4051-9045-ab2b9fe4fe5a 1 0 0 0 256b 256b
green open .xerj_cluster_state 524f93b5-3e73-4337-b68c-0f3049263c71 1 0 0 0 256b 256b
green open .xerj_audit 180ac644-f773-41c7-a3e8-554cf991fb2e 1 0 0 0 256b 256b
green open .xerj_views a24e4336-362f-4d19-8683-345989482ec1 1 0 0 0 256b 256b
green open .xerj_passkeys 22034dc4-971e-44e5-8596-a6bf28120ab7 1 0 0 0 256b 256b
green open .xerj_dashboards 08a529bb-25c9-4f28-8db9-c1e7e885e050 1 0 0 0 256b 256b
green open bench 6a37c084-5fac-414e-9adf-8f94d99ad298 1 0 4008 0 835239b 835239b
green open .xerj_alert_rules 9f4e92a4-fd71-4383-b497-3853b743fffe 1 0 0 0 256b 256b
green open .xerj_idp_config 067f5ff2-9429-431b-a6d2-adfa30ed729b 1 0 0 0 256b 256b
green open .xerj_alert_
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
  "_scroll_id": "f2efb43c-ea8e-4a28-8193-ab94cd570e21",
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
        "_id": "431431ae-1d7a-4078-ae28-d6d3a590ec83",
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

