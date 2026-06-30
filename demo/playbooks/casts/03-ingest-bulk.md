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
        "_id": "c22c6d77-a257-4abd-825d-8d9560e34340",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782800421526738,
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
        "_id": "70902f08-6f83-4391-b3f3-3ab73a632c66",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782800421526738,
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
        "_id": "9242f06e-bfa8-45b0-83b2-c5b44d3b2fd3",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782800421526739,
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
        "_id": "fe5e3af4-703b-4fa8-a08e-c3572b7a7e50",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782800421526739,
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
green open .xerj_magic_links e9ac9578-3753-4b79-8c80-20fd0846506a 1 0 1 0 3428b 3428b
green open .xerj_passkeys d4796bad-4732-4f31-a24e-25031665e4cd 1 0 0 0 256b 256b
green open .xerj_cluster_state e693578d-c788-444f-af47-d3ec3cf53a63 1 0 0 0 256b 256b
green open .xerj_dashboards adebaccb-5324-4153-9b03-fe4822f7fd47 1 0 0 0 256b 256b
green open .xerj_api_tokens 48e5b857-2277-48ce-b59b-dd3de9297992 1 0 0 0 256b 256b
green open .xerj_prefs 792ccf1c-facb-4445-92c3-047e1b22cd2d 1 0 0 0 256b 256b
green open .xerj_audit c70c7281-4ca4-421d-89d1-81fbb2fb45d6 1 0 0 0 256b 256b
green open .xerj_alert_fires f589be02-d0a0-41f3-bc70-fcdd98e2668f 1 0 0 0 256b 256b
green open .xerj_connections 899d11bd-a661-4117-a6f6-74f70711c40e 1 0 0 0 256b 256b
green open .xerj_views c4245884-45b9-42f6-8d34-0b14df012735 1 0 0 0 256b 256b
green open .xerj_sessions c68af00a-f1dd-4c8e-8d6e-91459cbf2192 1 0 0 0 256b 256b
green open .xerj_idp_config 318aa734-525c-4094-90c3-13ee68c1dcb3 1 0 0 0 256b 256b
green open .xerj_alert_rules 58b4436f-8f3a-4b45-99e4-f26ae38e5d7f 1 0 0 0 256b 256b
green open articles de429a9b-ef11-4d90-89d8-8e3f4e61ed95 1 0 5 0 8285b 8285b
green open .xerj_users d0442a81-f398-4390-b0b2-c44c41bc8c33 1 0 0 0 256b 256b
```

_HTTP 200_

