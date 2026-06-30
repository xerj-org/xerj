# Boot & health — one binary is up

_Use case doc: install / quickstart_

Prove the single binary is serving the ES-compatible API (:9200) and the native ops API (:8080).

### ✅ cluster identity (ES-wire)

```bash
curl -s "http://localhost:9200/"
```

```json
{
  "name": "local",
  "cluster_name": "xerj",
  "cluster_uuid": "xerj-cluster-0000-0000-0000-000000000000",
  "version": {
    "number": "8.13.0",
    "build_flavor": "default",
    "build_type": "tar",
    "build_hash": "00000000",
    "build_date": "2024-03-22T00:00:00.000Z",
    "build_snapshot": false,
    "lucene_version": "9.10.0",
    "minimum_wire_compatibility_version": "7.17.0",
    "minimum_index_compatibility_version": "7.0.0"
  },
  "tagline": "You Know, for Search"
}
```

_HTTP 200_

### ✅ cluster health is green

```bash
curl -s "http://localhost:9200/_cluster/health"
```

```json
{
  "active_primary_shards": 15,
  "active_shards": 15,
  "active_shards_percent_as_number": 100,
  "cluster_name": "xerj",
  "delayed_unassigned_shards": 0,
  "initializing_shards": 0,
  "number_of_data_nodes": 1,
  "number_of_in_flight_fetch": 0,
  "number_of_nodes": 1,
  "number_of_pending_tasks": 0,
  "relocating_shards": 0,
  "status": "green",
  "task_max_waiting_in_queue_millis": 0,
  "timed_out": false,
  "unassigned_primary_shards": 0,
  "unassigned_shards": 0
}
```

_HTTP 200_

### ✅ nodes listing

```bash
curl -s "http://localhost:9200/_cat/nodes?v"
```

```json
127.0.0.1 0 6 1 0.56 0.81 0.94 cdfhilmrstw * local
```

_HTTP 200_

### ✅ native health (:8080)

```bash
curl -s "http://localhost:8080/v1/health"
```

```json
{
  "data": {
    "status": "yellow",
    "index_count": 15,
    "total_docs": 4009,
    "version": "1.0.0-rc.1"
  },
  "took_ms": 0,
  "request_id": "cf7a2cf2-6eb3-4ab1-8afb-8005705fa879"
}
```

_HTTP 200_

### ✅ prometheus metrics (:8080)

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
# HELP xerj_docs_indexed_total Total number of documents successfully indexed
# TYPE xerj_docs_indexed_total counter
xerj_docs_indexed_total 0
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
xerj_flush_duration_seconds_bucket{le="2.56"} 0
xerj_flush_duration_seconds_bucket{le="5.12"} 0
xerj_flush_duration_seconds_bucket{le="10.24"} 0
xerj_flush_duration_seconds_bucket{le="20.48"} 0
xerj_flush_duration_seconds_b
… (4626 more bytes)
```

_HTTP 200_

