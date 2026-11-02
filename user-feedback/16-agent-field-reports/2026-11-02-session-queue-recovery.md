# Session handoff queue recovery (2026-11-02)

**Agent:** OpenAI Codex · **XERJ:** source-built xerj v1.0.0-rc.100 candidate for #2849 · **Platform:** macOS arm64

**Pointed at:** Synthetic Claude Code JSONL in temporary projects, plus missing, oversized, mismatched and corrupt queued deliveries; no personal transcripts.

**Used it for:** Verify installed session hooks, cited memory handoff, Unicode filtering and recovery after PR review.

**Verdict:** Before the fix, a bad queued item stopped sibling imports; the regression reproduced that failure. With the candidate, bad deliveries were retained with reasons while healthy work completed, and offline work resumed after restart. Moving the executable required reconfiguring hooks. I have not tested actual Claude Code host shutdown or a large session history.

**Numbers:** Both insecure and API-key smoke runs passed: 2 then 3 searchable records, one memory entry; 4 rejected deliveries retained with reasons. Complete autoindex tests: 2023 passed, 0 failed, 5 existing ignored, 3 existing macOS filters. These are integration checks, not a retrieval-quality or performance evaluation.

**Filed alongside:** https://github.com/xerj-org/xerj/pull/2849
