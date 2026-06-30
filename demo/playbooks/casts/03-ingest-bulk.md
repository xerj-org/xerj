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
        "_id": "01ddaa6c-b55e-4744-a7b2-538577fe3fe8",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782804896530799,
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
        "_id": "fe733455-65ba-4344-ba9d-098a7ba9de28",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782804896530800,
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
        "_id": "5a5e1107-526e-469c-8bdb-db19781d5b4a",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782804896530800,
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
        "_id": "d9fee133-5834-4a29-b4d1-a337ffaabd41",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782804896530800,
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
green open .xerj_views 31596b14-e712-4797-9929-85e10c54395d 1 0 0 0 256b 256b
green open .xerj_passkeys 93a50b07-aca9-47b2-b67c-055f194b4f14 1 0 0 0 256b 256b
green open .xerj_api_tokens 4ed82b27-d71d-47b4-9556-1d179859a3d8 1 0 0 0 256b 256b
green open bench 12b80aee-37d7-40e8-927e-df40bf6932df 1 0 4008 0 834617b 834617b
green open articles 13e3fe2d-968f-4d1c-82c0-cdbd98fb2622 1 0 5 0 7933b 7933b
green open .xerj_alert_rules 3da22893-0ea9-478f-9a88-65a435fde280 1 0 0 0 256b 256b
green open .xerj_magic_links 852b1d94-05ab-4acf-971b-e9cd0e86e6d8 1 0 1 0 3429b 3429b
green open .xerj_audit e200a5a8-cda4-41cc-84cb-fa105a6d7918 1 0 0 0 256b 256b
green open .xerj_users dba296ff-be53-4b28-9129-53bd8385f671 1 0 0 0 256b 256b
green open .xerj_idp_config 0fa56f1a-b887-44e8-ad98-3d20a4ed4742 1 0 0 0 256b 256b
green open .xerj_cluster_state 1d731ded-5d5d-4b03-895e-1b325cf38b40 1 0 0 0 256b 256b
green open .xerj_dashboards 9cba697e-f54c-46ba-a937-4ddcd5a440b7 1 0 0 0 256b 256b
green open .xerj_prefs 0629383d-20d7-4378-909e-e3ca771f073d 1 0 0 0 256b 256b
green open .xerj_sessions bc91342d-8e0e-4d16-b895-ef7dcfd59c6c 1 0 0 0 256b 256b
green open .xerj_alert_fires b109c826-ee71-41f4-9350-9e3898deb208 1 0 0 0 256b 256b
green open .xerj_connections 79c5216b-d56d-4aba-904f-38aa8cbc8099 1 0 0 0 256b 256b
```

_HTTP 200_

