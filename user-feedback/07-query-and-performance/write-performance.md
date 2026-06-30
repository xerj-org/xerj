# Write and Indexing Performance Issues

## Severity: MODERATE | Frequency: MODERATE

---

## Core Complaints

### Eventual Consistency
- Documents not searchable until next refresh cycle (default 1 second)
- Under load, refresh delays grow, creating windows where saved data is invisible
- Users cannot immediately find records they just created
- Causes duplicate-posting bugs when applications check for existing records after writing

### Indexing Throttling
- When merge can't keep up, ES throttles incoming indexing to single thread
- Intermittent drops in indexing rate
- Spiky CPU during merge storms

### Update API is Not In-Place
- Every update reindexes the entire document internally
- Frequent updates on large documents are extremely expensive
- Update API documented to take "weeks" for large-scale operations

### Bulk Indexing Sizing
- Optimal bulk request size is 5-15MB
- Depends on document size, cluster capacity, network conditions
- Too large = memory pressure; too small = round-trip overhead waste
- Finding sweet spot requires empirical testing per cluster
- ES bulk API uses NDJSON (newline-delimited JSON) -- text protocol with high parsing overhead

### Data Loss Risk During GC
- VM pause (GC, high IO) can cause loss of acknowledged inserts
- 10% loss rate documented in one failure pattern (GitHub #10933, #10426)

---

## XERJ.ai Response
- WAL-first architecture: writes durable immediately after fsync
- Memtable swap: writes continue during segment flush (no blocking)
- gRPC binary ingest: protobuf batches with zero-copy deserialization
- No JVM pause → no data loss from GC
- No in-place updates needed: append-only design with soft deletes

## Sources
- Tiger Data: 10 Production Issues
- Rockset/Medium: Performance Challenges
- GitHub Issues: #10933, #10426
