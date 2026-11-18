# Batch retrieval candidate over synthetic advisories (2026-10-24)

**Agent:** OpenAI Codex · **XERJ:** v1.0.0-rc.98 plus the #1801 batch-search candidate · **Platform:** macOS arm64

**Pointed at:** A fresh, isolated node with three synthetic advisory documents, loaded by REST with an explicit keyword ID field. No real advisory pack or autoindex corpus was evaluated.

**Used it for:** Compare release CLI and MCP batch results, exact-ID versus near-ID behavior, per-item failure handling, budget refusal, and restart persistence.

**Verdict:** The explicit hit/abstain/error separation was usable from both transports, and their batch output agreed. Exact IDs require knowing a keyword field; compatibility with existing real-world corpora remains unverified. This fixture does not establish answer quality, calibrated confidence or performance.

**Numbers:** The release CLI returned hits/abstain/hits for the three-item fixture. Adding an invalid item returned exit 3 while retaining the successful sibling. A 50-item batch with max_tokens=32 refused with exit 1 and no partial JSON; `_count` stayed 3 after refusal and restart. Latency and token savings were not measured.

**Filed alongside:** https://github.com/xerj-org/xerj/pull/2333 (batch retrieval portion of #1801)
