# OpenSearch vs Elasticsearch: What the Fork Reveals

## Both Projects Share Fundamental Limitations (XERJ.ai's Opportunity)

---

## Where OpenSearch Wins

| Area | OpenSearch Advantage |
|------|---------------------|
| **Licensing** | Apache 2.0, all features free (security, RBAC, audit logging) |
| **Vector dimensions** | Up to 16,000 (FAISS/nmslib) vs ES's 4,096 (Lucene only) |
| **Governance** | Linux Foundation, no single-company control |
| **OpenTelemetry** | Vendor-neutral first; ES pushes proprietary Elastic Agent |
| **AWS integration** | Native IAM, KMS, CloudWatch, Graviton optimization |
| **Independent benchmarks** | Trail of Bits: 1.6x faster on Big5, 16.55x faster date histograms |

## Where Elasticsearch Wins

| Area | Elasticsearch Advantage |
|------|------------------------|
| **Solutions** | Turnkey SIEM, APM, Maps, Lens -- OpenSearch has no equivalents |
| **Kibana vs Dashboards** | Polished UX; OS Dashboards has ~33% task success rate without help |
| **Development velocity** | 3x more commits on core; 14x more on modules (scripting, ingest) |
| **RAG/GenAI** | ESRE provides integrated RAG framework |
| **Tail latency** | 43x max-to-median ratio vs OpenSearch's 1,412x (much worse) |
| **Text queries** | 2.42x faster even in benchmarks where OS won overall |

## Where OpenSearch Is Worse

- Plugin maturity: ML, vector, security still maturing
- Cloud: AWS-only managed service (no Azure/GCP native)
- Community described as "not thriving" outside AWS
- Searchable snapshots: "a bit alpha"
- Migration back to ES requires near-complete rebuild (Elastic clients block OpenSearch)

---

## Problems BOTH Share (Lucene/JVM Constraints)

These are architectural limitations neither project can fix:

1. **JVM Heap Management**: GC thrashing at 75-90%, 31GB compressed oops ceiling, pauses triggering cascading failures
2. **Coupled Storage and Compute**: Can't independently scale; adding capacity = full nodes (CPU+RAM+disk)
3. **Lucene + Object Storage**: "Known for not being performant in object storage" -- both struggle to separate storage/compute
4. **Shard Management**: No good auto-tuning; requires deep expertise; wrong choice = reindex everything
5. **Cluster State Bottleneck**: Master processes changes serially; dynamic mapping bloats state
6. **Schema/Mapping Explosion**: "Schema-less" creates fields for every unique key
7. **Scaling Brownouts**: Rebalancing causes minutes-long performance degradation
8. **Deep Pagination Cliff**: Hard limit at 10,000; exponential memory beyond that
9. **Refresh/Merge Storms**: 1-second refresh creates segments faster than merge can consolidate
10. **Vector Search Memory Wall**: HNSW must be in RAM; disk spillover = dramatic degradation
11. **Exponential Cost Scaling**: Storage/compute costs scale super-linearly with data volume
12. **Specialized Expertise Required**: "Managing a cluster at scale is like managing a high-performance race engine"
13. **No True Serverless**: Both constrained by Lucene on object storage

---

## Community Sentiment

> "OpenSearch is a massively inferior offering compared to Elasticsearch" -- HN commenter

> "Both systems struggle with complexity: setup requires extensive configuration with no convention" -- HN

> "Contributors feel betrayed by Elastic but also see OpenSearch community as not thriving" -- HN

Users deploy OpenSearch "specifically and exclusively" for search/vector where licensing matters, but keep ES for observability/SIEM where the ecosystem is stronger.

---

## XERJ.ai Opportunity

The fork proves the market wants alternatives. But OpenSearch inherited ALL of ES's architectural debt:
- Same JVM, same GC pauses, same heap constraints
- Same Lucene, same segment model, same file proliferation
- Same shard management complexity
- Same storage-compute coupling

**XERJ.ai starts clean:** Rust (no JVM), custom storage engine (no Lucene), unified segment format (2 files, not 12+), AI-native from day zero. Every shared limitation above is a design constraint XERJ.ai doesn't have.

## Sources
- Trail of Bits: March 2025 ES vs OS benchmark
- Elastic: Performance benchmark claims
- Community discussions: HN, Reddit
- Various comparison articles (BigData Boutique, Pureinsights, SquareShift)
