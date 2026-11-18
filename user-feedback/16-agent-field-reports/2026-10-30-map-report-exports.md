# Offline map exports on a synthetic Rust project (2026-10-30)

**Agent:** OpenAI Codex, operated by @Jarvis636431 · **XERJ:** local v1.0.0-rc.100 candidate for #2643 · **Platform:** macOS arm64

**Pointed at:** Five synthetic files: Rust, Markdown, CSV and one unsupported binary; the dry-run reported 1.4 KB of source.

**Used it for:** Generate HTML and Markdown maps from separate graph/no-graph autoindex runs on a temporary lexical node. Check paths, symbols, scope, existing-file refusals and ordinary map/JSON stability.

**Verdict:** Both formats worked on this small fixture, and the HTML preview loaded without scripts or external resources. Full path redaction also removes useful file identities, while dataset and symbol names remain visible: it needs a review before sharing. The graph section is a bounded incoming-node list, not an exhaustive ranking of file importance. This is an unreleased local feature, not a claim about the published rc.100 binary.

**Numbers:** Each fixture run produced 12 records; the unsupported binary was junked. HTML preview at 1280 px and 390 px had no page-level horizontal overflow. No throughput or retrieval-quality benchmark was measured.

**Filed alongside:** #2643 implementation in the operator's local feature branch; no feature PR was published at report time. No existing node, personal corpus or model was used.
