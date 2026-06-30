# Vector Search and AI Limitations

## Severity: HIGH | Frequency: GROWING (rapidly)

---

## Core Complaints

### Architectural Mismatch
- Elasticsearch built for inverted indexes; vector search is a bolt-on, not native
- JVM creates significantly larger memory footprint vs C++/Rust-based dedicated vector DBs
- Architecture doesn't decouple write operations, index building, and querying
- Significant CPU/IO overhead during writes bottlenecks high-frequency update scenarios

### Performance Gaps vs Dedicated Vector DBs
- **30x latency disadvantage**: 1M vectors: ES ~200ms vs Milvus ~6ms (Zilliz benchmarks)
- **3x throughput gap**: Zilliz Cloud ~6,000 QPS vs ES Cloud ~1,900 QPS
- **15x slower data loading**: Zilliz Cloud loads/builds indexes 15x faster
- **Up to 68x worse latency** in some benchmark studies (Capella Solutions)
- **Vespa.ai**: 9x higher vector throughput, 5x faster hybrid search, 3x faster lexical search

### Dimension and Scale Limits
- Historically max 2,048 dimensions (raised to 4,096 in later versions)
- Milvus supports 32,768; Weaviate supports 65,535
- Similarity search slows significantly beyond ~50M vectors per index
- Fundamental limitations in scaling to billions of vectors

### Memory Requirements
- HNSW indexes require vectors held in memory
- 138.3M docs at 1024 dims: raw float storage exceeds 520GB RAM
- Embedding vectors dominate memory usage
- BBQ quantization and DiskBBQ are recent mitigations but trade speed for memory

### Missing Features (vs Dedicated Vector DBs)
- No disk-based indexes (DiskANN) until very recent versions
- No optimized metadata filtering for vector queries
- No range search functionality
- Eventual consistency only -- not immediately searchable after writes

### Hybrid Search Problems
- BM25 scores (0 to infinity) vs vector scores (0 to 1): incompatible ranges
- Lexical results dominate over semantic when interleaving without careful normalization
- Filters on kNN DECREASE performance (unlike conventional queries)
- Filters can yield zero results if top-k candidates don't match filter criteria
- `sub_searches` with `rank` requires commercial license

### AI Capabilities Immature
- "The AI capabilities, based on what is considered GA, is really, really subpar" -- Gartner Peer Insights
- "Constant drive to be first to market comes at a cost" -- AI features not mature despite "The Search AI company" rebranding
- "Elasticsearch focuses on exact keyword matching and does not address semantic similarity well" -- PeerSpot
- No native RAG support, no agentic memory, no inline embedding
- ELSER sparse model limited to 512 tokens

### Nested Document Restrictions
- Dense vectors cannot be declared in nested documents
- Nested kNN queries don't support filter specification

---

## Competitor Advantages

| Competitor | Key Advantage Over ES |
|-----------|---------------------|
| **Milvus** | C++ core, 30x lower latency, 3x higher throughput, 15x faster indexing |
| **Qdrant** | Rust-based, sophisticated filtering + vector similarity |
| **Weaviate** | Native hybrid search via BlockMax WAND + RSF; knowledge graphs |
| **Pinecone** | Fully managed, billions of vectors, consistent performance, minimal ops |
| **Vespa.ai** | 9x vector throughput, 5x hybrid search speed |

---

## XERJ.ai Response
- **Vector search is a first-class engine**, not a Lucene bolt-on
- Written in Rust: no JVM overhead, SIMD distance calculations
- HNSW with filter predicate pushdown INTO graph traversal (not post-filter)
- Quantization from day one: scalar (4/8-bit), binary (RaBitQ)
- Unified hybrid query planner: single cost model for FTS + vector
- Inline embedding: send text, get semantic search (no external pipeline)
- Auto-chunking with parent-child linkage for RAG
- Agentic memory index type with semantic dedup + recency weighting

## Sources
- Zilliz/Milvus: ES Was Great But Vector DBs Are the Future
- Capella Solutions: Vector DB vs Elasticsearch
- Vespa.ai: Elasticsearch Alternative Benchmark
- Gartner Peer Insights, PeerSpot reviews
- Elastic Forum: kNN Search Performance Issues
- Doug Turnbull: Elasticsearch Hybrid Search in Practice
