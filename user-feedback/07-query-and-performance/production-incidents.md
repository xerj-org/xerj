# Production Incidents and Postmortems

## Severity: CRITICAL | Source: Engineering blogs, GitHub, Jepsen

---

## Formal Testing: Jepsen/Aphyr (2014-2015)

### ES Loses Acknowledged Writes
Jepsen testing of ES 1.5.0 found:
- ~22% data loss during intersecting network partitions
- ~25% loss when a primary is isolated
- ~9.3% loss during GC pauses (SIGSTOP/SIGCONT)
- ~10% loss when nodes are killed

Root cause: custom ZenDisco consensus lacks proper coupling between failure detection and write replication. Translog only fsyncs every 5 seconds by default.

**Aphyr's conclusion:** "Store your data in a database with better safety guarantees, and continuously upsert every document from that database into Elasticsearch."

---

## Real-World Incidents

### Blinkit (Grofers): Dynamic Mapping Type Collision
- Backend API down November 25 at 4:30 AM
- `price` field had different types in different mappings within same index
- Lucene silently merged them at segment level, corrupting search results
- Team had believed ES was "schema-less"
- Fix: explicit mapping + `"dynamic": "strict"` + PostgreSQL fallback

### Tideways: 7-Hour OOM Outage
- Single index/single shard: disk surged from 40GB to 126GB in 4 days
- "Delete by query" + optimize was extremely resource-intensive
- Crashed with OutOfMemoryError and file descriptor exhaustion
- Fix: date-based indices, increased RAM from 12GB to 32GB/node
- Lesson: "Know your database. We have been using Elasticsearch naively."

### Plaid: Repeated Cluster Crashes Over 2 Weeks
- Data nodes died from JVM Memory Pressure spikes
- Aggregation queries creating 10,001+ buckets with millions of unique keys
- Circuit breaker estimation was inaccurate for these queries
- Fix: `search.max_buckets` = 10,000, lower circuit breaker limits

### GOV.UK: 3,000 Pages Corrupted After Upgrade
- Specialist pages (drug safety updates) disappeared after ES 2.4 upgrade
- 3-line code change removed `_type`/`_id` from source (reserved in 2.4)
- Other code still depended on these fields
- ~3,000 pages showed corrupted search results

### 1,200+ ES Databases Ransomed
- Automated scripts scanned for open ES instances (port 9200)
- Deleted all indexes, left ransom notes demanding 0.012 Bitcoin
- Root cause: no authentication by default

### 1.2 Billion Personal Records Exposed (2019)
- Unprotected ES database on Google Cloud
- Names, emails, phone numbers, social media profiles
- Data from People Data Labs and OxyData.io

### 8.7 Billion Chinese Records Leaked
- National ID numbers, home addresses, plaintext passwords
- No authentication on internet-facing cluster

### Radar: Replaced ES with Custom Rust Solution
- ES fanned queries to all shards, required service-orchestrated batch updates
- 1 billion+ API calls daily made these problems acute
- Built "HorizonDB" in Rust (RocksDB + S2 + Tantivy)
- Saved "high five-figures monthly"
- Forward geocoding at 50ms median; reverse geocoding <1ms
- 1,000 queries/second per core

### Twitter/X: Ingestion Pipeline Couldn't Keep Up
- During traffic spikes (breaking news), ES pipeline couldn't scale
- Safety mechanisms for spikes left to client, not ES

### Uber: Near-Real-Time Was Unacceptable
- Lucene NRT semantics: updates not searchable until flush
- For rider-driver matching, true real-time was required
- Built custom solution

### ES "Six Ways to Crash" (Self-Documented by Elastic)
1. Mapping explosion: 30K docs with unique fields consumed excessive heap
2. Too many shards: 1 billion shards = immediate OOM
3. Large size parameter: `Integer.MAX_VALUE` crashes node
4. Infinite scripts: consume all search threads
5. Deep aggregations: 6-level nested = OOM even without data
6. Long GC pauses: nodes lag, lose sync, risk data loss

### "Tragic Event in Index Writer"
- Critical Lucene-level error making shards permanently unrecoverable
- Triggered by: disk failure, filesystem corruption, file descriptor limits
- Recovery tool may cause data loss

### 90-Hour Recovery After Death Spiral
- Nasty query executed repeatedly → nodes unresponsive → memory leak → "death spiral of doom"
- Rebuilt entire cluster from scratch
- Recovery took 90 hours

---

## Forum Incidents

### 99-Node Cluster Degraded After 7.x Upgrade
- 85,000 shards averaging 300MB each
- Cluster repeatedly went yellow/red with data nodes dropping
- Elastic staff: "your average shard size is only 300 MB...very small and quite inefficient"

### Query Performance 2x Regression (7.9.2 → 7.17.2)
- P95 response: <20ms → >60ms
- CPU usage: 2x increase on data nodes
- Query cache hit rate: ~20% → ~0%
- **Thread closed unresolved after 28 days**

### OOM on One Node Killed Entire Cluster
- 3-node cluster, single shard with zero replicas
- One node OOM → entire cluster failed
- Planned go-live was imminent

### Upgrade Trap: Can't Go Forward OR Back
- Deprecated `dateOptionalTime` format in 7.x indices
- ES 8.x refused to start
- Data path already modified → downgrade also blocked
- Users left in unrecoverable state (GitHub #84199)

---

## XERJ.ai Response
- No JVM → no GC pauses → no GC-related data loss
- WAL with configurable fsync → no 5-second durability gap
- CRC32 checksums per block → corruption detected immediately
- Single-node → no split brain, no partition-related data loss
- Explicit schema (TOML) → no dynamic mapping surprises
- Rust memory safety → no OOM from buffer overflows
- Fuzz testing on compression codec → catch corruption before production

## Sources
- Aphyr: Jepsen Elasticsearch 1.5.0
- Tideways blog, Plaid blog, GOV.UK blog
- Secureworks, CISO Mag, Cybernews
- Radar blog, Twitter/X blog, Uber blog
- Elastic Blog: Six Ways to Crash
- Opster: Tragic Event in Index Writer
- HN: #33562359 (90-hour recovery)
- Elastic forum: #206843, #307889, #109207, #336665, #84199
