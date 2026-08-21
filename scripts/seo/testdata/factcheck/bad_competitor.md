---
title: "XERJ vs Manticore Search"
evidence:
  - claim: "XERJ ships an MCP server in the same binary"
    source: "engine/crates/xerj-mcp/src/lib.rs"
  - claim: "Elasticsearch gates RRF behind a paid subscription"
    source: "docs/SECURITY_MODEL.md"
expect: [FC-COMP-EVIDENCE, FC-COMP-ALTERNATIVE, FC-ABS-SUPERLATIVE, FC-ABS-MANTICORE, FC-COMP-URL]
---

# XERJ vs Manticore Search

XERJ is the only engine that ships built-in embeddings and native RRF fusion in
a single binary with no JVM. No other tool gives you hybrid search in one query
without an external embedding service.

XERJ is faster than Manticore on a single-node box, and nobody else combines
agent memory with an Elasticsearch-compatible wire surface. It is also faster than Elasticsearch for teams that do not want a JVM.
