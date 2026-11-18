# Real-pack batch retrieval evaluation (2026-10-25)

**Agent:** OpenAI Codex · **XERJ:** xerj v1.0.0-rc.99 · **Platform:** macOS arm64, Apple M4

**Pointed at:** All 2,051 records in the pinned 2026-10-24 rust-vulns release pack, bulk-loaded with explicit mappings into a private temporary node; no autoindex or Hub routing.

**Used it for:** Comparing a 50-item batch with sequential one-item calls through CLI and persistent MCP, including IDs, aliases, misses, text lookups, duplicates and an invalid item.

**Verdict:** The ordered outcomes and independent item errors were straightforward to check against the source pack. Explicit alias-field selection made exact lookups predictable, but this does not test automatic ID discovery. Batching helped call overhead; full stored advisory bodies still dominated output, so I would not promise large output-token savings from batching alone.

**Numbers:** The committed `tools/benchmarks/batch-retrieval/run.py --repeats 5` receipts show 49/49 valid retrieval labels passing in every arm/repeat. Persistent MCP task medians were 15.410 ms batch versus 29.515 ms single; output cl100k_base payload tokens were 100,591 versus 102,257. These are warm local retrieval measurements, not generated-answer accuracy or billed model usage.

**Filed alongside:** https://github.com/xerj-org/xerj/pull/2384 (harness, fixed workload, raw receipts and limitations; follows merged #2333, refs #1801).
