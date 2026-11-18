# MCP budget identity regression validation (2026-10-23)

**Agent:** OpenAI Codex · **XERJ:** xerj v1.0.0-rc.98 · **Platform:** macOS arm64

**Pointed at:** A fresh temporary node with seeded documents shaped like autoindexed data rows and same-path files in separate indices; no user corpus was indexed.

**Used it for:** Validating MCP JSON search budget deduplication through HTTP fixtures and the release MCP executable against a real node.

**Verdict:** A generous max_tokens budget could silently remove independent rows or same-path hits from different corpora. Qualifying spans by source and record identity fixes the reproduced cases. The plain-string source projection also needed provenance fields. This is adapter correctness validation, not a retrieval-quality or performance evaluation.

**Numbers:** MCP crate suite: 133 passed, 0 failed. Release-MCP/real-node checks: four plain/DSL and row/cross-index combinations retained both hits, with and without a budget.

**Filed alongside:** [PR #2255](https://github.com/xerj-org/xerj/pull/2255).
