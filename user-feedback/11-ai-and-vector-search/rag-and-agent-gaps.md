# RAG, Agent, and Modern AI Workload Gaps

## Severity: HIGH | Frequency: GROWING RAPIDLY

---

## What AI Builders Need vs What ES Provides

### Document Chunking for RAG
- **Need:** Split long documents into overlapping chunks, embed each chunk, query returns relevant chunks with parent document context
- **ES provides:** Nothing. Users must build chunking pipeline externally, manage parent-child relationships in application code, coordinate two writes (text + vector)
- **Pain:** Every RAG tutorial shows a custom chunking pipeline. It's the #1 boilerplate code in AI apps.

### Inline Embedding
- **Need:** Send text, get semantic search results
- **ES provides:** Must pre-compute vectors externally (call OpenAI/Cohere API → get vector → index into ES). Two separate systems, two failure modes.
- **Pain:** Every ingest pipeline needs an embedding service, error handling, retry logic, batch management

### Agent Memory
- **Need:** Append-only memory store with semantic dedup, recency-weighted retrieval
- **ES provides:** Nothing purpose-built. Users bolt together a vector index + custom scoring + application-level dedup
- **Pain:** Every agent framework (LangChain, LlamaIndex, CrewAI) builds this ad-hoc

### Hybrid Search (Keyword + Semantic)
- **Need:** Single query that blends BM25 keyword relevance with vector similarity
- **ES provides:** Separate KNN phase + BM25 phase, stitched together. Score normalization is broken (BM25: 0-infinity vs vector: 0-1). `sub_searches` with `rank` requires commercial license.
- **Pain:** Lexical results dominate. Filters on kNN DECREASE performance.

### Contextual Retrieval
- **Need:** Return not just matching chunks but surrounding context (previous/next chunks, parent document metadata)
- **ES provides:** Nothing. Application must make multiple queries or store context redundantly.
- **Pain:** RAG quality depends heavily on context windows. ES forces this into app code.

---

## What Gartner/Analysts Say

> "The AI capabilities, based on what is considered GA in the product right now, is really, really subpar."
> -- Gartner Peer Insights reviewer

> "The constant drive to be first to market comes at a cost" -- AI capabilities "not being mature despite the company rebranding itself as 'The Search AI company'"
> -- Gartner Peer Insights

> "Elasticsearch focuses on exact keyword matching and does not address semantic similarity well"
> -- PeerSpot reviewer

---

## What Competitors Provide That ES Doesn't

| Feature | Weaviate | Qdrant | Pinecone | ES |
|---------|----------|--------|----------|-----|
| Inline embedding | Yes (modules) | Yes (FastEmbed) | Yes (integrated) | No |
| Auto-chunking | No | No | No | No |
| Hybrid search (native) | Yes (BlockMax WAND + RSF) | Yes (sparse+dense) | Yes | Partial (broken scoring) |
| Agent memory primitives | No | No | No | No |
| Filtered ANN | Yes (pre-filter) | Yes (payload filter) | Yes (metadata filter) | Yes (but slower) |

**No search engine offers auto-chunking or agent memory.** This is XERJ.ai's unique opportunity.

---

## XERJ.ai Response

### Inline Embedding
```toml
[[fields]]
name = "content_embedding"
type = "vector"
source_field = "content"
embedding_model = "openai/text-embedding-3-small"
```
Send text → XERJ.ai embeds → indexes both. Atomic. No external pipeline.

### Auto-Chunking
```toml
[[fields]]
name = "body"
type = "chunk"
chunk_size = 512
chunk_overlap = 64
embedding_model = "openai/text-embedding-3-small"
```
Long document → chunks with overlap → each chunk embedded → parent linkage tracked.

### Agent Memory
```json
POST /v1/indices { "type": "memory", "config": { "dedup_threshold": 0.95, "recency_half_life": "7d" } }
```
Append-only. Semantic dedup. Recency-weighted retrieval.

### Unified Hybrid Search
Single query plan. Cost-based optimizer. RRF scoring. No broken score normalization.

## Sources
- Gartner Peer Insights, PeerSpot reviews
- Shaped.ai: 7 Best Elasticsearch Alternatives
- Firecrawl: Vector Databases Compared
- AIMultiple: Top Vector Database for RAG
- Doug Turnbull: Elasticsearch Hybrid Search in Practice
