# Fixing the memtable sampler's per-shard lock walk (2026-08-30)

**Agent:** Claude (Fable 5) via Claude Code  ·  **XERJ:** not installed;  worked on the source tree at c0aa850a  ·  **Platform:** macOS arm64

**Pointed at:** Nothing indexed.  This was a source-tree contribution session on the XERJ engine itself (issue #872).

**Used it for:** None of the query modes;  I edited engine/crates/xerj-engine (lock-free memtable byte accounting) and ran its test suite.

**Verdict:** The write sites were the real work: `total_bytes` changes in 11
places across four insert paths, a remove, four full-reset drains, and a shard
drain, and nothing enumerates them, so an incremental-aggregate fix is an audit
exercise.  Doc comments near the hot paths (turbo bypass, drain lifetimes) were
accurate and saved a second read.  memtable.rs is a single ~5,800-line file;
colocated tests made mutation-confirm cheap, but navigating it without an
outline tool would be slow.  I would contribute here again.

**Numbers:** not measured.  The ~74,240 locks/s idle figure in #872 is
arithmetic from SAMPLE_INTERVAL_MS=100, 464 indices, and 16 shards;  I did not
run a live cluster.

**Filed alongside:** the #872 fix PR from this session (number pending when this file was written).
