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
        "_id": "6e505def-8b8b-4048-a7bb-449a149f8af5",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782797511936383,
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
        "_id": "ac27e975-0932-45cf-92b1-3075721203a8",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782797511936384,
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
        "_id": "ccbacb35-a75d-4486-a030-177c5cb00ac5",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782797511936384,
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
        "_id": "f73700d7-04b1-4a58-9ea2-2eff09af1c66",
        "_index": "articles",
        "_primary_term": 1,
        "_seq_no": 1782797511936384,
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
green open .xerj_users 16684016-624c-4d3b-8f02-2af9c1328fe4 1 0 0 0
green open .xerj_prefs be018abd-cc75-456c-9215-9720f937ae48 1 0 0 0
green open .xerj_connections 65d22489-6e71-4e28-86a9-9c9ca1b17616 1 0 0 0
green open .xerj_magic_links 741ebaf0-6522-4066-bee8-c288bf69933c 1 0 1 0
green open .xerj_sessions cbf776a4-8847-436d-9151-2079ae833164 1 0 0 0
green open .xerj_views eff92923-d7ab-4311-95a8-07e2362ce77c 1 0 0 0
green open articles e1ca6d9c-f280-4189-9361-dceb95bab88d 1 0 5 0
green open .xerj_api_tokens 95708ab2-575c-43d6-9822-3ad8da08ebf2 1 0 0 0
green open .xerj_idp_config 37766ce7-06b9-4b7a-af7f-ff233f514924 1 0 0 0
green open .xerj_alert_fires 85733184-9cb6-4c15-ba32-5ef6a958849e 1 0 0 0
green open .xerj_passkeys 331b2a9f-549b-4405-8bf0-f25a8df65aee 1 0 0 0
green open .xerj_dashboards 12d74f66-2ae4-45f5-83f4-f4adf8af6d3d 1 0 0 0
green open .xerj_alert_rules 3a68a786-d7ca-4927-9306-0230415c5166 1 0 0 0
green open .xerj_audit 5254a3a0-a3b7-4b4a-81b7-b1d3faa9431c 1 0 0 0
green open .xerj_cluster_state b4d7b908-226d-4bdd-9ebc-de6eacaa3e65 1 0 0 0
```

_HTTP 200_

