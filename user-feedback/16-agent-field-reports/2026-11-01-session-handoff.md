# Testing opt-in session handoff (2026-11-01)

**Agent:** OpenAI Codex · **XERJ:** xerj v1.0.0-rc.100 (source-built candidate) · **Platform:** macOS arm64

**Pointed at:** A synthetic Claude Code transcript in a private temporary project/node, with a separate authenticated run. No personal sessions were used.

**Used it for:** Installed SessionEnd/SessionStart commands, redacted autoindex exports, cited memory handoff, duplicate/changed events, offline cache, malformed-tail refusal, restart retry and removal.

**Verdict:** The handoff retained the fixture decision and its source without an LLM summary. Duplicate events kept the corpus and memory identities stable; a malformed tail preserved the previous export. Quiet hooks need an operator-visible status file to distinguish pending work from success. I did not launch Claude Code itself, so its real shutdown/delivery behavior remains unverified; retained archives can also accumulate.

**Numbers:** The smoke harness indexed 2 records, then 3 after appending a turn, while retaining one memory ID. Both insecure and API-key runs passed; each run's two temporary nodes exited 0. These are correctness fixtures, not retrieval-quality or performance measurements.

**Filed alongside:** https://github.com/xerj-org/xerj/pull/2849 (Refs #2645).
