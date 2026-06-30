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
        "_id": "d11e7ede-b068-4147-af2e-deaa4a498842",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782803694944341,
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
        "_id": "957e3737-41b8-4cbc-96f5-19d96085af39",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782803694944341,
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
        "_id": "29a20531-8154-4dab-ada5-37fceb3c99ed",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782803694944341,
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
        "_id": "66f6e1d8-4b09-493c-ab02-a76975a4ca56",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782803694944341,
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
green open .xerj_connections cc8db897-d8c6-4c5a-a120-c9f6fb866b74 1 0 0 0 256b 256b
green open .xerj_dashboards 5eee38c7-1892-41bf-8e00-29f94ecb6e83 1 0 0 0 256b 256b
green open .xerj_api_tokens 74f2d1c3-712e-42bf-af21-9a03f53c7e34 1 0 0 0 256b 256b
green open bench 04ab71a6-4d1b-40ea-bc95-e25fb07d1d09 1 0 4008 0 834527b 834527b
green open .xerj_sessions 2634e065-2a08-4f10-9a9d-a71c64b3b4a2 1 0 0 0 256b 256b
green open .xerj_idp_config 0aa9cf4d-0221-482e-974f-69f10d979669 1 0 0 0 256b 256b
green open .xerj_passkeys e6bee4aa-4304-450e-bc9f-31332e315f25 1 0 0 0 256b 256b
green open .xerj_magic_links d8482b2a-5c5e-4fef-a952-611cb18d5ceb 1 0 1 0 3428b 3428b
green open .xerj_views 4cd23034-8e98-4392-adf7-01b38f22035a 1 0 0 0 256b 256b
green open articles 7574005a-910e-44d8-b366-f6d4334fed6e 1 0 5 0 7042b 7042b
green open .xerj_cluster_state ed15d284-086f-4a23-9035-83529d58c595 1 0 0 0 256b 256b
green open .xerj_users c9744a18-f65d-45ab-b4a7-817299027eff 1 0 0 0 256b 256b
green open .xerj_alert_rules 50c81aab-3a62-45c2-8b73-540815b34e56 1 0 0 0 256b 256b
green open .xerj_prefs 90574ba4-e7f1-4482-a2c0-556087a5dba6 1 0 0 0 256b 256b
green open .xerj_alert_fires fc3778d2-e6d7-4e25-a9bf-0e615e4a3b16 1 0 0 0 256b 256b
green open .xerj_audit 83e0bd9e-1f27-45b7-9e33-cddf9012fe5d 1 0 0 0 256b 256b
```

_HTTP 200_

