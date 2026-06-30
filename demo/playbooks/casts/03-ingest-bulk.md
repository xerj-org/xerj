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
        "_id": "cb1a87fa-b677-42f9-a436-c015b2d100e5",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782804405130211,
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
        "_id": "3b25929f-a384-40de-a07a-dd03262762dc",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782804405130211,
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
        "_id": "487b211e-b489-4821-87d6-312dab6571e4",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782804405130211,
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
        "_id": "b3356327-a8c6-487e-87a1-1e44d5245883",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782804405130211,
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
green open .xerj_alert_fires 9b9e0f5d-d5a5-4342-8a7f-9e8fbe52677c 1 0 0 0 256b 256b
green open .xerj_cluster_state 4bdda89b-45d6-474c-b7db-11d658fe82da 1 0 0 0 256b 256b
green open .xerj_api_tokens 5e8d2fe1-b8fa-412a-bf58-810b7de7d558 1 0 0 0 256b 256b
green open .xerj_sessions f9d1b505-1206-4403-90a0-b0492935feb2 1 0 0 0 256b 256b
green open .xerj_audit a1586adc-497a-45fb-b83f-5f046baecb9c 1 0 0 0 256b 256b
green open .xerj_passkeys a5a115d8-5fbe-4716-9c87-28c830334712 1 0 0 0 256b 256b
green open .xerj_magic_links e024997b-a885-4a04-84ef-34fa86a56fcd 1 0 1 0 3428b 3428b
green open .xerj_dashboards addf6a4f-8816-4e8c-9c8b-bb3e03e68e3b 1 0 0 0 256b 256b
green open .xerj_views 32165251-1ea9-4228-a1f6-f949cc2d256f 1 0 0 0 256b 256b
green open articles 32c2e153-40e8-409e-bec3-f85256dec7bc 1 0 5 0 8284b 8284b
green open .xerj_idp_config e3c04366-4162-45f7-b050-43d7155e5025 1 0 0 0 256b 256b
green open .xerj_prefs af3b8e70-4bef-4392-b14d-4e658b074962 1 0 0 0 256b 256b
green open .xerj_users e1aa692c-3675-4c2f-b961-6f7b2a2c6518 1 0 0 0 256b 256b
green open .xerj_alert_rules 468378b1-c5af-45d6-a800-4c037071f96a 1 0 0 0 256b 256b
green open bench 831f391f-ef04-4a53-bd4d-adb7e44b79bf 1 0 4008 0 834790b 834790b
green open .xerj_connections 590dbd1f-a2e6-4c91-90e9-cfb56066621e 1 0 0 0 256b 256b
```

_HTTP 200_

