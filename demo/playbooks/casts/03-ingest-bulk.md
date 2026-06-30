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
        "_id": "aab064f6-eecd-4b53-a75c-bf507f99edae",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782796632748670,
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
        "_id": "118126af-ff27-4207-9dd4-8f92a18e963e",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782796632748670,
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
        "_id": "93f7e5c2-629e-4cc2-a884-7c09d50f07f0",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782796632748670,
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
        "_id": "b4adabc9-73d7-4c67-a166-2b8c29556617",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782796632748670,
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
green open .xerj_cluster_state 13562e00-3e53-4abc-93e1-485daa33ab7d 1 0 0 0
green open .xerj_api_tokens 1b99a488-5e3d-4577-91f1-d550cebadd16 1 0 0 0
green open .xerj_prefs 3b2f09ba-b628-4709-892d-36838e7bcc95 1 0 0 0
green open articles 035aa409-ffd4-4900-bc11-f1ce1362704a 1 0 5 0
green open .xerj_dashboards bf82e004-fe5e-4e3b-981f-ae7f6d9761ea 1 0 0 0
green open .xerj_users f8914758-f9f4-4643-89f0-8cb1b6b60f7e 1 0 0 0
green open .xerj_passkeys c12ab029-b81d-4897-bb2e-c2b044fd55df 1 0 0 0
green open .xerj_alert_rules 0a55c41b-1850-4430-89cb-7b334bd32538 1 0 0 0
green open .xerj_magic_links 24ca4002-e3be-4fe2-908c-bd67d6f7114e 1 0 1 0
green open .xerj_idp_config 0b17a3f0-41d8-437b-b0ff-d8ed0b945830 1 0 0 0
green open .xerj_sessions 37411c10-15bc-45bc-a542-b99ec41dd52a 1 0 0 0
green open .xerj_audit 9f87223e-af48-489a-8c41-259e96006144 1 0 0 0
green open .xerj_connections a9d1fa85-c2c0-431b-87d4-e3019f093076 1 0 0 0
green open .xerj_alert_fires 14e14403-4005-4218-8494-c56c486ae3ae 1 0 0 0
green open .xerj_views bfce567b-a259-4083-a8cb-02d9604e2dd3 1 0 0 0
```

_HTTP 200_

