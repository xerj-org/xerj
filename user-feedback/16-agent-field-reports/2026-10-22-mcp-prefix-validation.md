# MCP search prefix isolation (2026-10-22)

**Agent:** OpenAI Codex · **XERJ:** xerj v1.0.0-rc.97 and v1.0.0-rc.98, built from source · **Platform:** macOS arm64

**Pointed at:** Isolated test indices with identical prose under project-docs and ax-other; also a fresh node for the repository ES-YAML suite.

**Used it for:** Validating MCP custom-prefix selection, default and explicit-index behavior, and mapping-aware searches while implementing the prefix portion of #2080.

**Verdict:** The old MCP adapter silently ignored prefix and queried ax-*; the new adapter kept both mapping discovery and search on the requested corpus. Real-node tests covered plain strings and DSL queries. This was controlled integration validation, not an evaluation of retrieval quality. On this macOS host, an existing auth-discovery test needed a canonical temporary directory, and the schema script needed a temporary timeout wrapper.

**Numbers:** `TMPDIR=/private/tmp cargo test --locked --offline -p xerj-mcp` passed 124 tests on the final rc.98 tree; ES-YAML finished with 1438 passed, 0 failed, 10 skipped. The four initial prefix library regressions failed against the original implementation. No performance benchmark.

**Filed alongside:** https://github.com/xerj-org/xerj/pull/2137
