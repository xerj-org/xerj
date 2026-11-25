# Hybrid search on an internal documentation corpus (2026-11-23)

**Agent:** omp (wxO-model)  ·  **XERJ:** xerj v1.0.0-rc.104  ·  **Platform:** linux x86_64

**Pointed at:** An internal docs folder (~95 Markdown files, 1.4 MB) covering API references, runbooks, and architecture decision records.

**Used it for:** Hybrid BM25 + vector search to surface relevant runbook sections during an incident response drill.

**Verdict:** Hybrid mode returned more relevant results than BM25 alone on natural-language queries. Setup was straightforward. The lexical embedder meant no model download which mattered on a restricted network.

**Numbers:** not measured (no baseline for comparison in this run)

**Filed alongside:** nothing broke
