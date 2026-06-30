# Native ops API (:8080) — health, metrics, flush, backup

_Use case doc: metrics / operations / api-native_

The native v1 ops surface: health + readiness probes, prometheus metrics, cluster health, a cluster-wide flush, and an on-disk backup (snapshot).

### ✅ native health

```bash
curl -s "http://localhost:8080/v1/health"
```

```json
{
  "data": {
    "status": "yellow",
    "index_count": 18,
    "total_docs": 4019,
    "version": "1.0.0-rc.1"
  },
  "took_ms": 0,
  "request_id": "4ea40768-d02f-400b-a41d-143a3b30a3a6"
}
```

_HTTP 200_

### ✅ readiness probe

```bash
curl -s "http://localhost:8080/v1/health/ready"
```

```json
ready
```

_HTTP 200_

### ✅ prometheus scrape

```bash
curl -s "http://localhost:8080/v1/metrics"
```

```json
# HELP xerj_active_searches Number of searches currently in flight
# TYPE xerj_active_searches gauge
xerj_active_searches 0
# HELP xerj_bytes_read_total Total bytes read from segment files (uncompressed)
# TYPE xerj_bytes_read_total counter
xerj_bytes_read_total 0
# HELP xerj_bytes_written_total Total bytes written to segment files (uncompressed)
# TYPE xerj_bytes_written_total counter
xerj_bytes_written_total 0
# HELP xerj_doc_count Total live documents across all indices
# TYPE xerj_doc_count gauge
xerj_doc_count 0
# HELP xerj_docs_indexed_by_index_total Documents indexed, labelled by index name
# TYPE xerj_docs_indexed_by_index_total counter
xerj_docs_indexed_by_index_total{index="articles"} 1
# HELP xerj_docs_indexed_total Total number of documents successfully indexed
# TYPE xerj_docs_indexed_total counter
xerj_docs_indexed_total 1
# HELP xerj_flush_duration_seconds Time to flush an in-memory buffer to a segment
# TYPE xerj_flush_duration_seconds histogram
xerj_flush_duration_seconds_bucket{le="0.01"} 0
xerj_flush_duration_seconds_bucket{le="0.02"} 0
xerj_flush_duration_seconds_bucket{le="0.04"} 0
xerj_flush_duration_seconds_bucket{le="0.08"} 0
xerj_flush_duration_seconds_bucket{le="0.16"} 0
xerj_flush_duration_seconds_bucket{le="0.32"} 0
xerj_flush_duration_seconds_bucket{le="0.64"} 0
xerj_flush_duration_seconds_bucket{le="1.28"} 0
xerj_flush_duration_seconds_bucket{le="2
… (5150 more bytes)
```

_HTTP 200_

### ✅ cluster health

```bash
curl -s "http://localhost:8080/v1/cluster/health"
```

```json
{
  "data": {
    "cluster_name": "xerj",
    "index_count": 18,
    "number_of_data_nodes": 1,
    "number_of_nodes": 1,
    "status": "yellow",
    "total_docs": 4019,
    "version": "1.0.0-rc.1"
  },
  "took_ms": 0,
  "request_id": "80c45115-cf0c-41d9-acca-00f68cc74229"
}
```

_HTTP 200_

### ✅ cluster-wide flush

```bash
curl -s -XPOST "http://localhost:8080/v1/admin/flush"
```

```json
{
  "data": {
    "failed": [],
    "flushed": 18,
    "indices": [
      "logs-2026-04",
      ".xerj_views",
      ".xerj_passkeys",
      ".xerj_api_tokens",
      "bench",
      "articles",
      ".xerj_alert_rules",
      ".xerj_magic_links",
      "vstore",
      ".xerj_audit",
      ".xerj_users",
      ".xerj_idp_config",
      ".xerj_cluster_state",
      ".xerj_dashboards",
      ".xerj_prefs",
      ".xerj_sessions",
      ".xerj_alert_fires",
      ".xerj_connections"
    ]
  },
  "took_ms": 6,
  "request_id": "b999b9fb-e47f-461f-9b09-dd3aaa83f9b8"
}
```

_HTTP 200_

### ✅ backup (snapshot to disk)

```bash
curl -s -XPOST "http://localhost:8080/v1/admin/backup" \
  -H 'content-type: application/json' \
  -d '{}'
```

```json
{
  "data": {
    "backup": "backup-2621b3cf-4f39-4a75-921c-4edaae8d5cd7",
    "manifest": {
      "duration_in_millis": 0,
      "end_time_in_millis": 1782804896898,
      "failures": [],
      "index_files": [
        {
          "files": [
            "snapshot.json",
            "schema.json",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.@timestamp.norms",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.@timestamp.meta",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.level.norms",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.level.meta",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.svc.norms",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.@timestamp.fst",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.svc.meta",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.msg.norms",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.msg.meta",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.level.fst",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.@timestamp.post",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.level.post",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.svc.fst",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.msg.fst",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.svc.post",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.msg.post",
            "791a01a6-3d15-4e69-bc01-9e3eba10f268.dv",
            "fa277a16-e134-4674-9d
… (92491 more bytes)
```

_HTTP 201_

