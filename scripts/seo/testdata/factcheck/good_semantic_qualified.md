---
title: "Does XERJ do semantic search?"
evidence:
  - claim: "The neural embedder is opt-in behind --embed-mode neural"
    source: "docs/EXPERIMENTAL_ONNX.md"
  - claim: "The default embedder is lexical feature hashing"
    source: "docs/EXPERIMENTAL_ONNX.md"
---

# Does XERJ do semantic search?

XERJ can do semantic search, but only when you opt in. Start the node with
`--embed-mode neural` and it downloads a MiniLM-class model and embeds your text
with it. The default embedder is lexical feature hashing: it has no model, it
cannot connect synonyms, and it will happily rank an unrelated sentence above an
exact synonym.

So: lexical by default, neural is opt-in. If synonym matching matters to you,
turn the neural embedder on and measure it on your own corpus first.
