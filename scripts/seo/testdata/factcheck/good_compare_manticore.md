---
title: "XERJ vs Manticore Search"
competitors: [manticore]
evidence:
  - claim: "Manticore ships local ONNX embedding models with no API key"
    source: "https://manual.manticoresearch.com/Searching/KNN"
  - claim: "Manticore fuses BM25 and KNN with OPTION fusion_method='rrf' in one query"
    source: "https://manual.manticoresearch.com/Searching/Hybrid_search"
  - claim: "XERJ publishes 1,366 of 1,369 ES-YAML conformance cases on a curated subset"
    source: "Tier A: .github/workflows/ci.yml"
---

# XERJ vs Manticore Search

Both are single binaries. Both run on one node with no JVM and no external
embedding service. Manticore ships local ONNX models and fuses BM25 with KNN
using RRF inside one query; XERJ does the same thing with its `hybrid` query
type, and its neural embedder is opt-in behind `--embed-mode neural` while the
default embedder is lexical feature hashing.

What XERJ adds is a published, machine-checked Elasticsearch conformance number:
99.8% of the ES-YAML suite, on a curated 200-file subset. It also adds
zero-config typed folder ingest, tree-sitter symbol extraction, and an agent
memory API with an MCP server in the same process. XERJ is Apache-2.0;
Manticore is GPL-3.0, which matters if you embed the engine in a commercial
product.

## When to choose Manticore instead

Choose Manticore if you want maturity: it has 21 years of Sphinx lineage against
XERJ's pre-1.0. Choose it if you need neural embedding breadth, because it ships
Qwen, Llama, Mistral and Gemma-class local models while XERJ's neural path is
MiniLM-class. Choose it if SQL is the interface your team wants.
