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
    "index_count": 17,
    "total_docs": 11,
    "version": "1.0.0-rc.1"
  },
  "took_ms": 0,
  "request_id": "5cdfecab-cb65-4485-abeb-ccf5ac92b38d"
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
… (5146 more bytes)
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
    "index_count": 17,
    "number_of_data_nodes": 1,
    "number_of_nodes": 1,
    "status": "yellow",
    "total_docs": 11,
    "version": "1.0.0-rc.1"
  },
  "took_ms": 0,
  "request_id": "401a3bfa-5151-4585-ba88-b6eea7fb8b70"
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
    "flushed": 17,
    "indices": [
      ".xerj_users",
      ".xerj_prefs",
      ".xerj_connections",
      ".xerj_magic_links",
      ".xerj_sessions",
      ".xerj_views",
      "articles",
      ".xerj_api_tokens",
      "vstore",
      "logs-2026-04",
      ".xerj_idp_config",
      ".xerj_alert_fires",
      ".xerj_passkeys",
      ".xerj_dashboards",
      ".xerj_alert_rules",
      ".xerj_audit",
      ".xerj_cluster_state"
    ]
  },
  "took_ms": 6,
  "request_id": "2bbde9cd-e4a8-4905-a778-06d71a2ec32b"
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
    "backup": "backup-4c0811fe-c67e-4343-988d-7f89817ab3d9",
    "manifest": {
      "duration_in_millis": 0,
      "end_time_in_millis": 1782797512303,
      "failures": [],
      "index_files": [
        {
          "files": [
            "0000000000000000.wchk",
            "0000000000000000.wal",
            "0000000000000000.wchk",
            "0000000000000000.wal",
            "0000000000000000.wchk",
            "0000000000000000.wal",
            "0000000000000000.wchk",
            "0000000000000000.wal",
            "0000000000000000.wchk",
            "0000000000000000.wal",
            "0000000000000000.wchk",
            "0000000000000000.wal",
            "0000000000000000.wchk",
            "0000000000000000.wal",
            "0000000000000000.wchk",
            "0000000000000000.wal",
            "0000000000000000.wchk",
            "0000000000000000.wal",
            "0000000000000000.wchk",
            "0000000000000000.wal",
            "0000000000000000.wchk",
            "0000000000000000.wal",
            "0000000000000000.wchk",
            "0000000000000000.wal",
            "0000000000000000.wchk",
            "0000000000000000.wal",
            "0000000000000000.wchk",
            "0000000000000000.wal",
            "0000000000000000.wchk",
            "0000000000000000.wal",
            "0000000000000000.wchk",
            "000000000000
… (37169 more bytes)
```

_HTTP 201_

