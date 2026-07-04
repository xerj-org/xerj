# Bulk ingest — ES /_bulk wire protocol

_Use case doc: ingest_

Point any ES collector (Logstash/Filebeat/SDK) at Xerj — /_bulk works unchanged.

### ✅ bulk index 4 docs

```bash
curl -s -XPOST "http://localhost:9200/articles/_bulk" \
  -H 'content-type: application/x-ndjson' \
  --data-binary $'{"index":{}}\n{"title":"A","body":"alpha vector","views":10,"tag":"a"}\n{"index":{}}\n{"title":"B","body":"beta search","views":20,"tag":"b"}\n{"index":{}}\n{"title":"C","body":"gamma logs","views":30,"tag":"a"}\n{"index":{}}\n{"title":"D","body":"delta agg","views":40,"tag":"b"}\n'
```

```json
{
  "errors": false,
  "items": [
    {
      "index": {
        "_id": "a42a64ea-0f2c-44b2-b4af-3db93b145cf3",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782855459332453,
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
        "_id": "e8953ed6-3cfa-411f-b585-c141cbb28f0f",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782855459332454,
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
        "_id": "7e9e9f67-1d1b-4dbb-a833-06052cb89393",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782855459332454,
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
        "_id": "f5644d8f-b189-4f4e-bbec-e5dbcc4ccaf8",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782855459332455,
        "_shards": {
          "failed": 0,
          "successful": 1,
          "total": 1
      
… (111 more bytes)
```

_HTTP 200_

### ✅ refresh to make searchable

```bash
curl -s -XPOST "http://localhost:9200/articles/_refresh"
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

### ✅ count documents

```bash
curl -s "http://localhost:9200/articles/_count"
```

```json
{
  "_shards": {
    "failed": 0,
    "skipped": 0,
    "successful": 1,
    "total": 1
  },
  "count": 5
}
```

_HTTP 200_

### ✅ cat indices

```bash
curl -s "http://localhost:9200/_cat/indices?v"
```

```json
green open .xerj_views c1085939-2ee4-4c41-84f0-27c8b4055587 1 0 0 0 256b 256b
green open .xerj_passkeys 34737b04-ab94-4c61-921a-c60e0a22bb55 1 0 0 0 256b 256b
green open .xerj_sessions c7f50939-f76f-4a8d-a123-54c14c04ff0b 1 0 0 0 256b 256b
green open .xerj_dashboards 7c555883-6604-4697-84bc-6ff2c9a6e17c 1 0 0 0 256b 256b
green open articles 6d2b5789-63af-46e7-905d-ae8dfae790d1 1 0 5 0 8284b 8284b
green open .xerj_idp_config 554ed071-9c2e-47af-8980-5a5080155506 1 0 0 0 256b 256b
green open .xerj_alert_rules 2298aee0-3b18-45d1-bac1-3840853af691 1 0 0 0 256b 256b
green open .xerj_connections 834427c7-47cf-4a43-ab48-9f41a9120fc0 1 0 0 0 256b 256b
green open .xerj_cluster_state 97013a17-d8d0-4a54-bcb1-2c9b47afe9b2 1 0 0 0 256b 256b
green open .xerj_alert_fires 3d76692a-d8c0-49b1-a69b-aea7440024cd 1 0 0 0 256b 256b
green open .xerj_prefs 4aca1cac-4822-4c01-b36b-a812a27847c3 1 0 0 0 256b 256b
green open .xerj_magic_links 4274efef-5f1e-464c-aa02-ba74f792b486 1 0 1 0 3429b 3429b
green open .xerj_users 1900ef80-d755-40cc-bb3b-efd7e809455d 1 0 0 0 256b 256b
green open bench c1992e47-4719-4f8f-9918-0f00a6ce8b61 1 0 4008 0 849235b 849235b
green open .xerj_audit afc96921-442b-4598-a6e9-c5d0d3478152 1 0 0 0 256b 256b
green open .xerj_api_tokens 23ba90e9-8efa-4c1a-a6d7-8b1985a9058e 1 0 0 0 256b 256b
```

_HTTP 200_

