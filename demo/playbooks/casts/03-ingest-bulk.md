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
        "_id": "431431ae-1d7a-4078-ae28-d6d3a590ec83",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782801644999388,
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
        "_id": "dab7f220-9dfa-4063-bb3e-832cfe92f457",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782801644999389,
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
        "_id": "042bd161-dd5d-40af-8cf1-66e87ed664a7",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782801644999389,
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
        "_id": "c1d8c0d8-7568-48e5-ae3c-b1dd05493320",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782801644999389,
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
green open .xerj_api_tokens d9cb84d8-8f8f-4f4c-87a4-ff1755068f3f 1 0 0 0 256b 256b
green open .xerj_magic_links 048c1bcd-9662-4ca0-afb3-b47859978e5c 1 0 1 0 3429b 3429b
green open .xerj_connections 6067b91b-1d8e-4a6c-a65c-71390a586857 1 0 0 0 256b 256b
green open .xerj_sessions 083df9aa-ba1e-446e-b086-bff0d0c3d046 1 0 0 0 256b 256b
green open .xerj_prefs 22499dab-fcd5-4aef-a373-3651fc33dc4f 1 0 0 0 256b 256b
green open articles bc19eafd-9b7c-4fb2-b1d3-df17a038df61 1 0 5 0 7050b 7050b
green open .xerj_users 47823f13-d53f-48ea-9e5a-1ce739432274 1 0 0 0 256b 256b
green open .xerj_cluster_state 091b1f09-ff6a-4822-b3f4-caddd650ac9a 1 0 0 0 256b 256b
green open .xerj_audit d82504c9-14df-4617-a587-4e945096ad79 1 0 0 0 256b 256b
green open .xerj_views 8da34997-dd1e-4d92-835a-11cbbdb6e027 1 0 0 0 256b 256b
green open .xerj_passkeys 0ad6c5e1-0f83-4ae4-b5bb-5c373634f00d 1 0 0 0 256b 256b
green open .xerj_dashboards ef26f9fb-7674-4b10-aa01-fe6beecb532a 1 0 0 0 256b 256b
green open bench 3dfff88f-0a84-4551-bc99-7a13177e6c45 1 0 4008 0 835239b 835239b
green open .xerj_alert_rules b40de605-b482-44d2-96e7-c4510cae5d13 1 0 0 0 256b 256b
green open .xerj_idp_config 3539b519-6add-4a09-a7c0-64725825843e 1 0 0 0 256b 256b
green open .xerj_alert_fires 6a04fa6a-d3b9-4ac0-ab46-307e1e28c09c 1 0 0 0 256b 256b
```

_HTTP 200_

