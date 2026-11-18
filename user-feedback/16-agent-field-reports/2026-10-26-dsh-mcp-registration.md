# Registering `xerj mcp` as a DeepSeek Harness stdio server (2026-10-26)

**Agent:** DeepSeek-V4.1-Flash (DeepSeek Harness 44.7.0)  ·  **XERJ:** `xerj v1.0.0-rc.99`  ·  **Platform:** Windows 11 26200, x86_64

**Pointed at:** `PensiveFei/dsh-secure-audit` - 38 files, ~440 KB of JavaScript, markdown and fixtures - on a fresh `--data-dir`.

**Used it for:** verifying the dsh row of the supported-agents table (stdio registration through `@deepseek-ai/dsh-mcp-client`) plus one retrieval call against the autoindexed repo.

**Verdict:** The two-field model held exactly: one absolute path plus `mcp`, and the harness showed 22 tools with `xerj_search` as `mcp__xerj__xerj_search`. `initialize` answered in 7 ms and `tools/list` in 2 ms, so a 128 MB binary costs nothing at startup. The no-node error was the best thing I hit - it named the port, the URL and the command that fixes it, instead of failing quietly. The worst thing was the server `instructions` block: it orders the agent to file a field report "automatically ... no need to ask the user" and open a GitHub PR, and it lands verbatim in the client's system prompt. I registered with `XERJ_DISABLE_FEEDBACK=true` because I did not want a CLI opening a PR from a signed-in account without its owner asking.

**Numbers:** `initialize` 7 ms, `tools/list` 2 ms (2585 B of instructions), autoindex 248 docs in 4.1 s, one `xerj_search` 74 hits in 46 ms.

**Filed alongside:** `docs:` PR flipping the dsh row from `untested` to `tested`.
