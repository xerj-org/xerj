# Native ops API (:8080) — health & metrics

_Use case doc: metrics / operations / api-native_

The native v1 surface for ops. NOTE: this binary implements health + metrics; the rest of the documented /v1 API is not yet live (captured as gaps).

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
  "request_id": "d64f1a4e-098c-4b04-8e97-586c978341c2"
}
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

### ❌ DOC GAP: /v1/health/ready

```bash
curl -s "http://localhost:8080/v1/health/ready"
```

```json

```

_HTTP 404_

### ❌ DOC GAP: /v1/cluster/health

```bash
curl -s "http://localhost:8080/v1/cluster/health"
```

```json

```

_HTTP 404_

### ❌ DOC GAP: /v1/admin/flush

```bash
curl -s -XPOST "http://localhost:8080/v1/admin/flush"
```

```json

```

_HTTP 404_

### ❌ DOC GAP: /v1/admin/backup

```bash
curl -s -XPOST "http://localhost:8080/v1/admin/backup" \
  -H 'content-type: application/json' \
  -d '{}'
```

```json

```

_HTTP 404_

