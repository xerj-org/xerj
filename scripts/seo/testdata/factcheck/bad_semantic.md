---
title: "Does XERJ do semantic search?"
evidence:
  - claim: "XERJ ships a semantic query type"
    source: "engine/crates/xerj-query/src/parser.rs"
expect: [FC-SEMANTIC, FC-MEANING, FC-EMBED-DEFAULT]
---

# Does XERJ do semantic search?

XERJ understands the meaning of your query. Ask for "car" and it will find
documents about automobiles, because semantic search matches concepts rather
than keywords.

Semantically similar documents rank together out of the box, with no model to
download and no configuration to write.
