#!/usr/bin/env python3
"""
Generate ai_kb.ndjson — a small enterprise-AI knowledge base for the XERJ demo.

Each document carries an 8-dimensional cosine embedding whose components map to
discrete enterprise-AI topic axes. Vectors are deterministic and normalized, so
kNN queries written by the runbook return predictable nearest neighbors:

    dim 0 — rag patterns / retrieval design
    dim 1 — vector index internals (HNSW, quantization)
    dim 2 — hybrid search (BM25 + vector fusion)
    dim 3 — ai ops / observability / latency
    dim 4 — security / compliance / data residency
    dim 5 — cost / tco / infra consolidation
    dim 6 — migration / es-replacement / pinecone exit
    dim 7 — agent memory / session state / long-context

Run:    python3 generate_ai_kb.py > ai_kb.ndjson
Result: 40 documents, one JSON object per line.

NOTE: 8 dims is intentional — it matches the benchmark schema XERJ ships with
(see engine/reports/2026-04-25T22-50-00_xerj_vs_elasticsearch_rerun.md). Real
production deployments use 384/768/1536 from sentence-transformers / OpenAI /
Voyage. The demo deliberately keeps vectors human-readable so SEs can show the
audience exactly why a kNN result ranks where it does.
"""

import json
import math
import sys
from datetime import datetime, timedelta

# (title, content, category, axis_weights)
DOCS = [
    # --- RAG patterns ---
    ("Chunking strategies for enterprise RAG",
     "Fixed-size chunking is a starting point, not a strategy. Production RAG pipelines benefit from semantic chunking with overlap, parent-document retrieval, and contextual headers prepended at index time.",
     "rag", [0.9, 0.2, 0.3, 0.1, 0.1, 0.2, 0.1, 0.2]),
    ("Why naive RAG fails at enterprise scale",
     "Naive top-k retrieval breaks down when the corpus has duplicate, near-duplicate, and stale passages. Re-ranking and deduplication move from optional to mandatory above ten million chunks.",
     "rag", [0.95, 0.3, 0.4, 0.2, 0.1, 0.1, 0.1, 0.1]),
    ("Multi-tenant RAG with per-customer indices",
     "Hard-isolating tenants by index gives the cleanest blast-radius story for compliance auditors and the simplest mental model for capacity planning. Aliases let a single query hit a tenant subset.",
     "rag", [0.85, 0.1, 0.2, 0.1, 0.6, 0.2, 0.1, 0.1]),
    ("Recursive RAG: when one retrieval pass is not enough",
     "Multi-hop questions require multi-hop retrieval. Generate sub-queries from the original question, fan out, and merge with a reciprocal-rank-fusion or learned re-ranker.",
     "rag", [0.9, 0.2, 0.6, 0.1, 0.1, 0.1, 0.1, 0.3]),
    ("Citation-first RAG for regulated industries",
     "When auditors ask which document a recommendation came from, the answer cannot be reconstructed from a vector. Persist source URLs, chunk IDs, and retrieval scores alongside every generated answer.",
     "rag", [0.85, 0.1, 0.2, 0.1, 0.85, 0.1, 0.1, 0.1]),

    # --- Vector index internals ---
    ("HNSW vs IVF vs DiskANN: choosing the right index",
     "HNSW dominates for sub-second p95 on under 100M vectors. IVF wins when memory is constrained. DiskANN matters above one billion vectors when SSD becomes the only place that fits.",
     "vector", [0.2, 0.95, 0.2, 0.4, 0.0, 0.4, 0.1, 0.1]),
    ("Scalar quantization: 4x memory savings for free",
     "SQ8 quantization compresses each dimension from float32 to uint8. Recall drops by less than half a point on most embedding models. SQ4 doubles the savings for noisier corpora.",
     "vector", [0.1, 0.95, 0.1, 0.3, 0.0, 0.7, 0.1, 0.1]),
    ("Why product quantization is making a comeback",
     "PQ fell out of fashion when IVFSQ8 became the default, but with billion-scale vector workloads on commodity hardware, PQ's 16-32x compression is the only thing that fits.",
     "vector", [0.1, 0.9, 0.1, 0.2, 0.0, 0.8, 0.2, 0.1]),
    ("Cosine vs dot product vs L2: similarity metrics that matter",
     "OpenAI and Voyage embeddings are L2-normalized, so cosine and dot product give identical rankings. Use dot product to skip a normalization pass per query.",
     "vector", [0.2, 0.9, 0.2, 0.3, 0.0, 0.2, 0.1, 0.1]),
    ("HNSW parameter tuning: M, ef_construction, ef_search",
     "M=16 and ef_construction=200 are safe defaults. Raising ef_search at query time trades latency for recall without rebuilding the graph.",
     "vector", [0.1, 0.95, 0.2, 0.4, 0.0, 0.2, 0.1, 0.1]),

    # --- Hybrid search ---
    ("Why pure vector search underperforms on names and SKUs",
     "Embedding models smear exact identifiers across dense space. A search for SKU-12345 should match the literal token, not its semantic neighborhood. Hybrid search fixes this.",
     "hybrid", [0.3, 0.3, 0.95, 0.2, 0.1, 0.1, 0.1, 0.1]),
    ("Reciprocal rank fusion: the simplest hybrid scoring",
     "RRF takes ordinal rank from BM25 and ordinal rank from kNN, sums the reciprocals, and re-ranks. No score normalization, no learned weights, predictable behavior.",
     "hybrid", [0.2, 0.2, 0.95, 0.2, 0.0, 0.1, 0.1, 0.1]),
    ("Single-pass hybrid: scoring lexical and vector together",
     "When the engine fuses BM25 and HNSW scores in one query plan, you skip the round-trip cost of two independent searches and the merge becomes O(k) instead of O(n).",
     "hybrid", [0.2, 0.4, 0.95, 0.5, 0.0, 0.3, 0.1, 0.1]),
    ("Boosting strategies for hybrid relevance",
     "Multiply BM25 by 1.0 and kNN by 0.8 as a starting boost ratio. Tune on a held-out evaluation set. Never tune on the corpus you trained the embedding model on.",
     "hybrid", [0.3, 0.3, 0.9, 0.3, 0.0, 0.1, 0.1, 0.1]),
    ("Filtered hybrid search: post-filter vs pre-filter",
     "Post-filtering kills recall when filters are selective. Pre-filtering during HNSW traversal is the only correct answer at production scale, but requires the index to support it.",
     "hybrid", [0.2, 0.6, 0.85, 0.4, 0.1, 0.2, 0.1, 0.1]),

    # --- AI ops / observability ---
    ("p95 latency budgets for interactive RAG agents",
     "If the LLM call is 800ms, retrieval has 200ms before perceived latency tanks. Anything over a second on retrieval makes the agent feel broken regardless of answer quality.",
     "ops", [0.4, 0.2, 0.3, 0.95, 0.1, 0.1, 0.1, 0.2]),
    ("Cold start latency in vector databases",
     "Restart latency matters more than steady-state throughput when you autoscale. A search engine that takes 15 seconds to warm cannot be scaled in under a minute.",
     "ops", [0.1, 0.5, 0.2, 0.95, 0.1, 0.4, 0.2, 0.1]),
    ("Observability for retrieval pipelines",
     "Log every query, the top-k ids returned, the score distribution, and the chunk source. Without retrieval traces you cannot tell whether a hallucination is a model bug or a context bug.",
     "ops", [0.3, 0.1, 0.2, 0.9, 0.2, 0.1, 0.1, 0.2]),
    ("Tail latency under concurrent load",
     "Median latency under load is a vanity metric. p99 is what your customers feel. Measure under k-times-expected concurrency, not under benchmark-friendly serial queries.",
     "ops", [0.1, 0.3, 0.3, 0.95, 0.0, 0.2, 0.1, 0.1]),
    ("Memory floor: the silent capacity planning trap",
     "An idle search node holding a billion vectors should not cost 64GB of resident memory. JVM-based engines treat memory as a single pool; native engines free what they are not using.",
     "ops", [0.1, 0.5, 0.1, 0.9, 0.0, 0.7, 0.3, 0.1]),

    # --- Security / compliance ---
    ("Data residency for LLM-grounded retrieval",
     "EU data must not cross into US-hosted vector databases without an SCC. The embedding is derived data; regulators treat it as personal data when reidentification is feasible.",
     "compliance", [0.3, 0.2, 0.1, 0.1, 0.95, 0.1, 0.1, 0.1]),
    ("PII in embeddings: the inversion attack",
     "Recent research shows embeddings can be partially inverted to recover the source text. Treat embedding stores like databases of personal data, not opaque vectors.",
     "compliance", [0.2, 0.5, 0.1, 0.1, 0.95, 0.1, 0.1, 0.1]),
    ("Audit logging for AI retrieval",
     "Every retrieval that grounds a customer-facing answer must be reproducible six months later. Store query, retrieved IDs, and snapshot version. Append-only, signed, retained per policy.",
     "compliance", [0.3, 0.1, 0.1, 0.3, 0.95, 0.1, 0.1, 0.1]),
    ("SOC 2 controls that apply to vector workloads",
     "Access control, change management, encryption at rest, encryption in transit, and incident response apply to retrieval infrastructure exactly the same way they apply to your primary database.",
     "compliance", [0.1, 0.2, 0.1, 0.2, 0.95, 0.2, 0.1, 0.1]),
    ("Tenant isolation under shared-infrastructure RAG",
     "Cross-tenant retrieval leakage is the highest-severity bug class in multi-tenant RAG. Filter at the index level, not in the application layer, and verify with chaos tests.",
     "compliance", [0.4, 0.2, 0.2, 0.2, 0.9, 0.1, 0.1, 0.1]),

    # --- Cost / TCO ---
    ("Why managed vector DBs get expensive at scale",
     "Per-vector pricing models look attractive at the proof-of-concept stage and become unsupportable at production scale. At a billion vectors, managed-DB bills routinely exceed the team that built the product.",
     "cost", [0.1, 0.5, 0.1, 0.2, 0.1, 0.95, 0.5, 0.1]),
    ("Calculating TCO: storage, search, vector together",
     "Run the math on the unified cost: hot search cluster, cold log archive, vector database, embedding API. Fragmented stacks compound cost; a single store collapses three line items into one.",
     "cost", [0.1, 0.3, 0.2, 0.2, 0.1, 0.95, 0.5, 0.1]),
    ("Embedding cost is a fixed cost, search cost is variable",
     "You pay to embed once. You pay to search forever. Optimize search-side first; the embedding bill amortizes within a quarter, the search bill is the rest of your career.",
     "cost", [0.2, 0.4, 0.3, 0.3, 0.0, 0.95, 0.2, 0.1]),
    ("JVM heap is a hidden tax on retrieval workloads",
     "Half your search cluster's memory budget goes to GC headroom you will never use. Native engines reclaim that overhead; the cost difference shows up as headcount, not just hardware.",
     "cost", [0.1, 0.3, 0.1, 0.5, 0.0, 0.95, 0.4, 0.1]),
    ("Disk efficiency for billion-document corpora",
     "Modern compression (Zstd-19 at the segment level) gives 3-4x reductions on log and document data without measurable query-time penalty. SSD lasts twice as long; backup windows halve.",
     "cost", [0.1, 0.4, 0.1, 0.4, 0.0, 0.9, 0.2, 0.1]),

    # --- Migration / replacement ---
    ("Migrating from Elasticsearch: what breaks and what works",
     "Standard query DSL works unchanged in 98% of cases. The 2% that breaks tends to be x-pack features, painless scripts, and cluster-shape assumptions. Plan for a four-week test phase, not a weekend cutover.",
     "migration", [0.1, 0.3, 0.4, 0.2, 0.2, 0.5, 0.95, 0.1]),
    ("Pinecone exit strategy for cost-pressured teams",
     "Export vectors via the Pinecone API, ingest into a self-hosted index, run shadow traffic for two weeks, compare recall@k and latency. The annual savings are typically larger than the migration cost.",
     "migration", [0.2, 0.6, 0.3, 0.3, 0.1, 0.85, 0.95, 0.1]),
    ("Decommissioning a Splunk SIEM: the realistic path",
     "Move new sources to the new store first; let the Splunk index age out per retention policy. A six-month dual-write period costs less than the all-at-once cutover and removes the rollback risk.",
     "migration", [0.1, 0.1, 0.2, 0.5, 0.4, 0.7, 0.95, 0.1]),
    ("Drop-in compatibility: the 80/20 of search migration",
     "If your client libraries point at port 9200 and speak ES query DSL, eighty percent of the migration is done. The remaining twenty percent is x-pack-shaped and worth scoping carefully up front.",
     "migration", [0.1, 0.1, 0.3, 0.2, 0.2, 0.4, 0.95, 0.1]),
    ("Dual-write patterns for zero-downtime cutover",
     "Write to both old and new stores during the migration window. Read from old, validate against new asynchronously. Cut over reads only when the diff between systems is steadily zero.",
     "migration", [0.1, 0.2, 0.2, 0.5, 0.2, 0.4, 0.95, 0.1]),

    # --- Agent memory ---
    ("What is agent memory, really?",
     "Agent memory is retrieval over a session, a user, and an organization, on three different time horizons. Treat it as three indices, not one, with three different retention policies.",
     "agent_memory", [0.3, 0.2, 0.3, 0.2, 0.1, 0.1, 0.1, 0.95]),
    ("Long-context windows do not replace memory",
     "A million-token context is still a context, not memory. Memory persists across sessions. Context resets every turn. Build retrieval from memory into the context; do not assume context is durable.",
     "agent_memory", [0.4, 0.2, 0.3, 0.2, 0.1, 0.1, 0.1, 0.95]),
    ("Per-user memory namespaces and the right to be forgotten",
     "Memory tied to a user must be deletable on request within the regulator's deadline. Index per user, not per agent, so deletion is a single operation rather than a fleet-wide sweep.",
     "agent_memory", [0.2, 0.2, 0.2, 0.2, 0.7, 0.1, 0.1, 0.95]),
    ("Episodic vs semantic memory for agents",
     "Episodic memory is the transcript of what happened. Semantic memory is the distilled fact. Production agents need both, indexed differently, retrieved by different queries, retained for different durations.",
     "agent_memory", [0.5, 0.3, 0.4, 0.2, 0.1, 0.1, 0.1, 0.95]),
    ("Memory consolidation: when to summarize, when to keep raw",
     "Raw episodic memory grows unboundedly. Summarize after 30 days; keep summaries forever. Anchor summaries to the original chunks with foreign keys for the regulator who eventually asks.",
     "agent_memory", [0.4, 0.3, 0.3, 0.3, 0.3, 0.2, 0.1, 0.95]),
]


def normalize(vec):
    """L2-normalize a vector so cosine similarity is well-defined."""
    n = math.sqrt(sum(x * x for x in vec))
    return [x / n for x in vec] if n > 0 else vec


def main():
    base_date = datetime(2026, 4, 1)
    sources = ["docs.internal", "engineering-blog", "kb.public", "wiki.private"]

    for i, (title, content, category, weights) in enumerate(DOCS, start=1):
        embedding = normalize(weights)
        doc = {
            "id": i,
            "title": title,
            "content": content,
            "category": category,
            "embedding": embedding,
            "source": sources[i % len(sources)],
            "indexed_at": (base_date + timedelta(days=i)).isoformat() + "Z",
            "word_count": len(content.split()),
            "in_scope": True,
        }
        sys.stdout.write(json.dumps(doc) + "\n")


if __name__ == "__main__":
    main()
