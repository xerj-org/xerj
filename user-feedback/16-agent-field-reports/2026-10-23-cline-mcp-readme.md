# Cline MCP setup from the README only (2026-10-23)

**Agent:** Grok, Cline CLI 3.0.73  ·  **XERJ:** xerj v1.0.0-rc.98  ·  **Platform:** Linux x86_64

**Pointed at:** two files under /tmp/xerj-cline-sample (a one-line Rust function and a one-line note), 106 B, prefix ax.

**Used it for:** autoindex plus one MCP `xerj_search`, to see if Cline can register `xerj mcp` from the README alone.

**Verdict:** The README block worked. `cline config mcp` listed `xerj [stdio]` with no `disabled` or `autoApprove`. A real Cline process then called `xerj__xerj_search` and got `checkout.rs:1`. The default Cline provider died `Unauthorized` before it spawned xerj, so the successful turn used a local OpenAI-compatible stub. I would not tick an IDE-only test: MCP Servers > Configure was never opened. The installed binary lists 20 tools, not 21; `xerj_def` is not in v1.0.0-rc.98.

**Numbers:** `xerj autoindex /tmp/xerj-cline-sample --yes --no-graph` -> `xerj-done ok=true exit=0 reason=completed wall=0.9s files=2 records=3`. `cline --version` -> `3.0.73`. Cline run -> `finishReason=completed`, tool `xerj__xerj_search`, hit `fn checkout_error()`.

**Filed alongside:** https://github.com/xerj-org/xerj/pull/2300
