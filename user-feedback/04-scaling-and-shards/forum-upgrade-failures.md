# Forum: Upgrade Failures and Performance Regressions

## Severity: CRITICAL | Frequency: HIGH across versions

---

## Performance Regressions After Upgrade

### 99-Node Cluster Degraded on 7.x (Forum #206843)
- 90 data nodes, 25,000-35,000 docs/sec
- After upgrading from 6.7.2 to 7.x: cluster repeatedly went yellow/red
- 85,000 shards averaging only 300MB each
- Elastic staff: "your average shard size is only just over 300 MB...very small and quite inefficient"

### Query 2x Slower on 7.17.2 (Forum #307889)
- P95 response: <20ms → >60ms
- CPU usage: 2x increase on data nodes
- Query cache hit rate: ~20% → ~0%
- **Thread closed UNRESOLVED after 28 days**

### Indexing 10-20% Slower on 7.10 (Forum #267938)
- 10-15% storage savings came with 10-20% indexing time degradation
- **Thread closed UNRESOLVED** with no workaround offered

### 5x Resource Usage After 5.6 → 6.7 Upgrade
- 48-CPU servers running at 80-90% after upgrade
- GC collection count increased 5x
- Old generation collections lasting 21.3 seconds
- "Moving data around was a pain and normal operations took ages to finish"
- Source: e-mc2.net GC Hell blog

### Query 6.6x Slower After 7.4 → 7.10 Upgrade (GitHub #67574)
- Small index (~4,500 docs)
- Query times: 89ms → 590ms with `size: 10000`
- Root cause: Lucene changed stored field block reading from 16KB to 60KB

### Boolean Query 5x Slower After 8.7 → 8.13 (GitHub #108659)
- `create_weight` step took 2-5x longer
- "It makes my production query about 5x times slower as my query has nearly a hundred 'should' clauses"
- Regression introduced around ES 8.12.0

---

## Upgrade Traps (Can't Go Forward OR Back)

### dateOptionalTime Trap (GitHub #84199, OPEN)
- Deprecated format in 7.x indices
- ES 8.x refused to start with `IllegalStateException`
- Data path already modified → downgrade also blocked
- Users left in unrecoverable state
- Elastic engineer DaveCTurner confirmed the catch-22

### Incompatible 6.x Indices Block 8.x (Forum #336665)
- Attempted 7.17.4 → 8.5.2 upgrade
- Hit: "cannot upgrade -- incompatible indices created with version [6.4.3]"
- Had to downgrade back to 7.17.4 and run Kibana Upgrade Assistant first

### Circuit Breaker "Data Too Large" After 6.1 → 6.2 (GitHub #31197)
- Shard recovery broke with memory limit errors
- Elastic: "improved memory accounting" in 6.2.0 -- not a new bug
- Users experienced it as a breaking change (32 comments)

### Keystore Trap on Failed 8.0 Upgrade (GitHub #81326)
- ES fails to start with unmigrated 6.8 system indices
- Keystore already modified to 8.x format
- Rollback blocked

### Rolling Upgrade 503 on 2-Node Cluster (Forum #364907)
- ES 8.8 → 8.15: resulted in 503 error
- Cluster left in degraded state

---

## XERJ.ai Response
- **Single binary upgrade.** Replace file, restart. Done.
- Segment format has version field in header for compatibility
- No Lucene version coupling
- No shard rebalancing during upgrade
- No cluster coordination needed
- No data path format changes that block rollback
- No JVM version requirements

## Sources
- Elastic Forum: #206843, #307889, #267938, #336665, #364907
- GitHub: #67574, #108659, #31197, #84199, #81326
- e-mc2.net: Elasticsearch Garbage Collection Hell
