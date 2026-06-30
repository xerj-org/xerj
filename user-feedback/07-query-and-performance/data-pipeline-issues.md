# Data Pipeline and Integration Issues

## Severity: HIGH | Frequency: HIGH in data engineering contexts

---

## Dual-Write / Source-of-Truth Problems

### ES Cannot Be Source of Truth
- Jepsen testing: 33-90% of acknowledged writes LOST during network partitions
- Worst case: 484 of 538 acknowledged writes (90%) vanished after single partition
- ZenDisco protocol lacked monotonic epochs: nodes could support two leaders simultaneously
- Expert advice: "Store data in a database with better safety guarantees, continuously upsert into Elasticsearch"

### Silent Data Divergence
- GoCardless: if app dies after DB commit but before ES sync job enqueued, update permanently lost from ES
- Even with transactional queues, network partitions cause same failure
- Race conditions: older updates can overwrite newer ones due to out-of-order arrival

---

## CDC and Sync Problems

### No Native CDC
- ES provides no recovery log or change stream
- Must rely on Debezium, PGSync, custom listeners
- Each adds operational complexity and failure modes

### Debezium Pipeline Issues
- Without `ExtractNewRecordState` SMT, raw CDC envelope indexed as-is (unusable "junk data")
- Throughput limited to ~1,800 inserts/second
- Mapping mismatches between source schema and ES mapping cause silent drops

---

## Kafka Connector Failures

### Silent 70-90% Data Loss
- Confluent connector GitHub #632: one user received only 10% of data in ES while S3 got 100%
- Root cause: flush timeout errors not surfaced prominently in logs

### Version Conflicts Cause Data Loss
- Multiple topics + UPSERT: concurrent writes to same doc ID produce conflicts
- Default: log warning and silently discard conflicting update
- GitHub #374

### Workers Stop Silently
- Workers process 200K-300K of 1M records then stop with no error in logs
- Requires manual investigation and restart

---

## Eventual Consistency Breaks Workflows

### 1-Second Refresh Gap
- Documents not searchable until next refresh (default 1s)
- Multi-step workflows: write status then query → sees stale data
- "Completed" steps appear as "pending"; pipelines stuck in loops

### Forcing Refresh Kills Throughput
- `refresh=true` or `refresh=wait_for`: 2-3x slower indexing
- Impossible trade-off between consistency and throughput

### Replica Lag
- Query may hit replica that hasn't received latest write
- Document exists on primary but returns "not found" from replica
- By design, not a bug

---

## Bulk Indexing Failures

### Rejected Requests Drop Data
- `es_rejected_execution_exception` when write queue full: entire bulk request rejected
- Without client-side retry + dead letter queue: documents simply lost

### Partial Failures Are Normal
- 1,000-doc bulk: 990 succeed, 10 fail (mapping conflicts, version conflicts)
- Many client implementations fail to check per-item status
- Failed documents silently lost

---

## Ingest Pipeline Limitations

### No Native Dead Letter Queue
- Unlike Logstash, ingest pipelines have no built-in DLQ (GitHub #86170, unimplemented)
- Must use `on_failure` handlers to manually route to "dead letter index"

### Date Processor Bottleneck
- Consumed more time than all other processors combined in benchmarks (GitHub #73918)
- Caused task queue backlogs and rejected requests under high throughput

### No Processor IDs
- Unlike Logstash named stages, processors have no IDs
- Errors reference sequential position only -- debugging complex pipelines is guesswork

---

## Reindexing Pain

### Full Reindex for Any Schema Change
- Field types immutable after indexing
- text → keyword, string → date: create new index + reindex all data
- Large indices: hours to days

### Writes During Reindex = Data Loss
- Writes to old index: never reach new index
- Writes to new index for unmigrated docs: fail silently
- "Outdated versions" persist permanently

---

## Transform and Enrich Limitations

### Transforms Can't Detect Deletions
- When source indices deleted (rolling time-based), entities NOT removed from destination
- Must recreate entire transform to clean up stale data

### Enrich Policies: Immutable and Slow
- Cannot be modified after creation
- 140M+ record index: ~6 hours to execute
- No native scheduling mechanism

---

## Version Conflicts at Scale

### Cascading Conflicts in High-Write Pipelines
- Multiple pipeline workers updating same doc: VersionConflictEngineException
- `retry_on_conflict` adds latency, doesn't guarantee success

### update_by_query Unreliable on Active Indices
- Snapshots data at query start, uses internal versioning
- Any doc modified between snapshot and update: skipped silently

---

## Scroll Context Exhaustion

### "Too Many Scroll Contexts"
- Default limit: 500 open contexts
- Easily exhausted by ETL jobs, data exports, poorly closed clients
- When hit: memory and CPU spike 4x
- Critical operations (update_by_query) fail entirely

---

## XERJ.ai Response
- WAL-first with configurable fsync: no 5-second durability gap
- gRPC streaming ingest: binary protocol, zero-copy, no NDJSON parsing overhead
- Version map with optimistic concurrency (like ES LiveVersionMap but lock-free DashMap)
- Native batch processing: atomic document + vector indexing
- No scroll context limits (cursor-based pagination)
- No schema migration reindexing: segment versioning
- Built-in dead letter handling for ingest failures

## Sources
- Jepsen: Elasticsearch (Aphyr)
- GoCardless: Syncing Postgres to ES
- GitHub: Confluent connector #632, #374
- GitHub: ES #86170, #73918
- codecentric: Zero Downtime Reindexing
- Elastic Docs: Transform limitations, Bulk API
- Elastic Blog: Keeping ES in Sync
