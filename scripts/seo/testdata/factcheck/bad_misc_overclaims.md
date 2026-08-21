---
title: "What can the XERJ agent memory and code index do?"
evidence:
  - claim: "xerj brain builds a graph over indexed documents"
    source: "engine/crates/xerj-api/src/memory_api.rs"
expect: [FC-KG, FC-CALLGRAPH, FC-MEMORY-DECAY, FC-RECALL-HYBRID, FC-LEARNED-FUSION, FC-AGG-10X, FC-DECAY-SCORING, FC-SEMANTIC-MATCH, FC-ML, FC-HOSTED, FC-TERRAFORM, FC-MSRV, FC-SINGLE-NODE]
---

# What can the XERJ agent memory and code index do?

XERJ builds a knowledge graph over your repository and understands the
relationships between symbols. Its call graph lets you find all the callers of a
function across files, because XERJ understands your codebase.

Agent memory has a forgetting curve: entries decay over time and importance
weighting keeps the useful ones. `_recall` uses hybrid RRF fusion over both
lexical and vector signals, and you can turn on `fusion: "learned"` to train a
reranker on your own click data.

Aggregations are often 10× faster, and gauss decay functions let you boost
recent documents. A `match` query on a `semantic_text` field is upgraded to a
vector search automatically.

Built-in anomaly detection and machine learning jobs watch your indices.

Deploy the managed service with our Terraform module. The minimum supported
Rust version is 1.94.
