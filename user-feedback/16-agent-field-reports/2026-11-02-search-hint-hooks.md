# Exercising advisory Claude Code search hints (2026-11-02)

**Agent:** OpenAI Codex · **XERJ:** xerj v1.0.0-rc.100 (source-built candidate) · **Platform:** macOS arm64

**Pointed at:** A private synthetic Rust project plus its generated AGENTS guide: 2 files, about 1.4 KB, lexical mode; no existing corpus was used.

**Used it for:** Installing and invoking PreToolUse search hints, autoindex/map/definition lookup, node restart and removal, and two controlled Claude Code 2.1.278 sessions.

**Verdict:** Quota, scope, offline silence and removal worked in the tested fixture. Carrying the configured node URL in the suggested command mattered for a custom port. The actual-host sample explicitly asked the agent to follow a delivered hint; it says nothing about spontaneous adoption or broad retrieval quality. Catalog-only readiness and the 200-run cap remain limits.

**Numbers:** Both insecure and authenticated smoke commands passed 26 installed hook invocations each (exit 0, empty stderr); each corpus held 3 records. The controlled host sample made 0 then 1 XERJ search calls with hints disabled/enabled.

**Filed alongside:** [#2869](https://github.com/xerj-org/xerj/pull/2869), with the runnable smoke harness and actual normalized host receipt.
