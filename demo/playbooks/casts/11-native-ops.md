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
  "request_id": "4d170a85-ad72-4b75-896f-b4a494e12b1c"
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
  "request_id": "df3bfedc-64d1-4a7a-95de-4b60c7602ff1"
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
      ".xerj_magic_links",
      ".xerj_passkeys",
      ".xerj_cluster_state",
      ".xerj_dashboards",
      ".xerj_api_tokens",
      ".xerj_prefs",
      ".xerj_audit",
      ".xerj_alert_fires",
      "vstore",
      ".xerj_connections",
      ".xerj_views",
      ".xerj_sessions",
      ".xerj_idp_config",
      ".xerj_alert_rules",
      "articles",
      ".xerj_users",
      "logs-2026-04"
    ]
  },
  "took_ms": 6,
  "request_id": "c1139269-c04d-40bf-b9df-8cace417acac"
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
    "backup": "backup-cd885b8b-aaae-4a98-85bb-ccbbf5713ddf",
    "manifest": {
      "duration_in_millis": 0,
      "end_time_in_millis": 1782800421896,
      "failures": [],
      "index_files": [
        {
          "files": [
            "snapshot.json",
            "schema.json",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.created_by.norms",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.used_at.norms",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.expires_at.norms",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.used_from_ip.norms",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.created_by.meta",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.used_at.meta",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.expires_at.meta",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.purpose.norms",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.used_from_ip.meta",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.email.norms",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.role.norms",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.purpose.meta",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.email.meta",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.role.meta",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.created_at.norms",
            "e9a268ac-63da-4a3a-a09d-fbdb00b092d9.user_id.norms",
            "e9a268ac-63da-4a3a-a09d-
… (37169 more bytes)
```

_HTTP 201_

