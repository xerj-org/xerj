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
        "_id": "55e198e7-da9e-42ec-bf14-b12c60b4f2a6",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782803097197382,
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
        "_id": "bd70d34e-3464-4626-95da-d938c3768835",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782803097197383,
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
        "_id": "e7f19229-eb41-42ee-a95b-a1975fa46192",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782803097197383,
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
        "_id": "1c04850f-3e99-4d2d-9b3b-9f21273966c6",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782803097197383,
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
green open .xerj_cluster_state 0e2f9d1f-09fb-4685-a723-c2c544a68e87 1 0 0 0 256b 256b
green open .xerj_sessions b1c86e43-60e9-4f0b-ae4a-172eaa4b3a01 1 0 0 0 256b 256b
green open .xerj_alert_rules 399dded0-d7e7-4510-b4bc-298909965aa9 1 0 0 0 256b 256b
green open .xerj_audit fc65e834-21eb-4d19-a324-ba2cc1e101ec 1 0 0 0 256b 256b
green open bench baea92ce-93f5-4a6f-9e09-03bf11d33643 1 0 4008 0 834761b 834761b
green open .xerj_prefs 82d1a681-af59-479e-bd87-8e48fa93493a 1 0 0 0 256b 256b
green open .xerj_dashboards 71042ba0-3d2c-42ff-8a03-feeb04b29075 1 0 0 0 256b 256b
green open .xerj_api_tokens a4f76df0-7461-41b0-8169-0e2ae8d6f3b1 1 0 0 0 256b 256b
green open .xerj_views 49cc29d4-37d2-4b93-8e33-f61e41840923 1 0 0 0 256b 256b
green open .xerj_users f2a600c0-839c-4bfc-aa3e-e279ff88169d 1 0 0 0 256b 256b
green open .xerj_passkeys 75e41db5-d5d4-4d86-98dd-99b82be7740c 1 0 0 0 256b 256b
green open .xerj_alert_fires 537a3648-d390-4fd1-bfc6-2d72ab34a66d 1 0 0 0 256b 256b
green open .xerj_idp_config 66cea3a7-dcb0-4bfe-ae37-c1c678b9f9d5 1 0 0 0 256b 256b
green open .xerj_magic_links 34eaedee-6226-40a7-b9bf-1e163b964e6f 1 0 1 0 3428b 3428b
green open articles 2340f8a4-38d8-450f-9453-8c791d576e45 1 0 5 0 5713b 5713b
green open .xerj_connections 54616806-20b8-4051-8f43-096727157375 1 0 0 0 256b 256b
```

_HTTP 200_

